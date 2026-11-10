//! components/skill-invocation-message.ts。
//!
//! `[skill]` 可折叠块：collapsed 单行，expanded 显示完整技能正文。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/skill-invocation-message.ts`。
//!
//! ## Public API for InteractiveMode
//!
//! - [`SkillInvocationMessageComponent::new`]（skillBlock, markdown theme, outputPad）
//! - `set_expanded` / `set_output_pad` / `Component::{render,invalidate}`
//!
//! ## 这个组件为什么存在
//!
//! 用户消息若是一个 skill 调用，会话层会把它包成 `<skill name=… location=…>正文</skill>`
//! 块，由 `kiana_session::agent_session::parse_skill_block` 解析成
//! `ParsedSkillBlock`（字段见该结构体：`name`、`location`、`content`、`user_message`）。
//! 本组件只使用 `name` 与 `content`。若照原样把整段正文当普通用户消息渲染，聊天记录会被
//! 技能全文淹没，也无法折叠。本组件把技能块渲染成一条可折叠行：收起时只显示
//! `[skill] 名称 (快捷键 to expand)`，展开时显示名称标题与完整正文（Markdown 渲染）。
//! 背景色沿用 `customMessageBg`，与其它自定义消息视觉一致。
//!
//! 技能块**之后**的可选用户消息（`ParsedSkillBlock::user_message`）不由本组件渲染：
//! 调用方 `interactive_mode::render` 会另建 `UserMessageComponent`，本组件只负责技能块
//! 本体（对应 TS 注释 “Only renders the skill block itself”）。
//!
//! ## 调用方与交互
//!
//! * 构造：`interactive_mode::render` 为每条含 skill 块的用户消息创建一次，并按宿主的
//!   `tool_output_expanded` 初始化展开状态；随后经
//!   `extension_ui::widgets::register_expandable` 登记到弱引用表，供全局“展开/收起”开关
//!   批量调用 `set_expanded`。
//! * 设置页：`interactive_mode::commands::settings` 在 `output_pad` 变化时通过
//!   `as_any_mut` 下转到本类型并调用 `set_output_pad`。
//! * 鼠标：整块区域包在一个 `MouseRegion` 里，左键点击即切换展开状态。
//!
//! ## 状态与重绘（为什么有 `dirty`）
//!
//! 展开状态存放在 `Rc<Cell<bool>>` 中而不是普通字段，因为点击回调是 `'static` 闭包，
//! 只持有 `Rc` 句柄，拿不到 `&mut self`。回调只能翻转 `expanded` 并置 `dirty`；真正的
//! 重建发生在下一次 `render` 开头（`dirty` 为真时调用 `update_display`）。因此点击后
//! 必须有后续渲染才能看到变化——备用屏对 Click 事件的默认行为就是请求重绘
//! （`kiana-tui/src/alt_screen.rs` 的 `apply_mouse_dispatch_result`：`Click` 在
//! `result.render` 未指定时按 `default_render = true` 处理）。
//!
//! ## 失败模式
//!
//! * 主题未初始化时 `fg` / `bg` 直接 panic（与 TS 访问未初始化主题抛错一致）。
//! * `set_output_pad` 只改 Box 的横向留白，不重新构建内容，因此不会清空/重建子项；
//!   下一次 `render` 按新留白渲染。
//! * 非左键点击、非 `Click` 事件一律返回 `None`，事件继续向外冒泡。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/skill-invocation-message.ts`。

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use kiana_session::agent_session::ParsedSkillBlock;
use kiana_theme::theme_tokens::{ThemeBg, ThemeColor};
use kiana_tui::components::box_component::BoxComponent;
use kiana_tui::components::markdown::{DefaultTextStyle, Markdown, MarkdownTheme, StyleFn};
use kiana_tui::components::mouse_region::MouseRegion;
use kiana_tui::components::text_component::Text;
use kiana_tui::tui::{
    Component, Container, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType,
};

/// 主题前景色；主题未初始化时 panic（与 TS 行为一致）。
fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// 主题背景色；主题未初始化时 panic（与 TS 行为一致）。
fn bg(color: ThemeBg, text: &str) -> String {
    kiana_theme::THEME
        .bg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// `class SkillInvocationMessageComponent extends Box`。
///
/// 外壳是 [`BoxComponent`]（负责背景色与左右的 output padding），唯一子项是包住内容的
/// `MouseRegion`。每次展开状态变化都整体 `clear` 再重建，因此不存在“局部更新”的路径。
pub struct SkillInvocationMessageComponent {
    /// 承载背景与留白的外壳；`children` 里只有一个 `MouseRegion`。
    bx: BoxComponent,
    /// 技能块数据（名称、路径、正文）；构造后不再变化。
    skill_block: ParsedSkillBlock,
    /// 渲染展开态正文用的 Markdown 主题。
    markdown_theme: MarkdownTheme,
    /// 展开状态；与点击回调共享（回调没有 `&mut self`）。
    expanded: Rc<Cell<bool>>,
    /// 内容需要重建：点击回调置位，下一次 `render` 开头消费。
    dirty: Rc<Cell<bool>>,
}

impl SkillInvocationMessageComponent {
    /// `constructor(skillBlock, markdownTheme = getMarkdownTheme(), outputPad = 1)`。
    ///
    /// `output_pad` 对应 TS 的 `super(outputPad, 1, …)` 的第一个参数，即**横向**留白；
    /// 纵向固定为 1 行。背景用 `ThemeBg::CustomMessageBg`。构造末尾立即调用
    /// `update_display`，所以对象一创建就有内容（初始为收起态）。
    pub fn new(
        skill_block: ParsedSkillBlock,
        markdown_theme: MarkdownTheme,
        output_pad: usize,
    ) -> Self {
        let mut component = SkillInvocationMessageComponent {
            bx: BoxComponent::new(
                output_pad,
                1,
                Some(Arc::new(|text: &str| bg(ThemeBg::CustomMessageBg, text))),
            ),
            skill_block,
            markdown_theme,
            expanded: Rc::new(Cell::new(false)),
            dirty: Rc::new(Cell::new(false)),
        };
        component.update_display();
        component
    }

    /// `setExpanded(expanded)`（无变化守卫）。
    ///
    /// 无论值是否变化都会重建内容：`update_display` 内部先置 `dirty = false` 再 `clear`
    /// 并重建，所以重复设置同一值只会白白重建一次，不会产生错误状态。
    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded.set(expanded);
        self.update_display();
    }

    /// `setOutputPad(outputPad)`：只改 paddingX。
    ///
    /// 不重建内容、也不置 `dirty`；`BoxComponent::set_padding_x` 自身会丢弃渲染缓存，
    /// 下一次 `render` 按新留白输出。此方法由设置页 `output_pad` 变更时对所有已挂载消息
    /// 组件统一调用（`interactive_mode::commands::settings` 的 `set_output_pad`，它通过
    /// `as_any_mut` 下转到本类型）。
    pub fn set_output_pad(&mut self, output_pad: usize) {
        self.bx.set_padding_x(output_pad);
    }

    /// 按当前 `expanded` 重建唯一子项 `MouseRegion`。
    ///
    /// 两种形态互斥：展开态 = `[skill]` 标签 + `**名称**\n\n正文` 的 Markdown；收起态 =
    /// 单行 `[skill] 名称 (快捷键 to expand)`，快捷键文本来自 `key_text("app.tools.expand")`。
    /// 每次重建都会新建 `MouseRegion`，并把 `expanded` / `dirty` 的 `Rc` 克隆进点击闭包。
    fn update_display(&mut self) {
        self.dirty.set(false);
        self.bx.clear();
        let mut content = Container::new();

        if self.expanded.get() {
            // Expanded：label + 技能名标题 + 完整正文。
            let label = fg(ThemeColor::CustomMessageLabel, "\x1b[1m[skill]\x1b[22m");
            content.add_child(Box::new(Text::with_padding(label, 0, 0)));
            let header = format!("**{}**\n\n", self.skill_block.name);
            let color: StyleFn = Arc::new(|text: &str| fg(ThemeColor::CustomMessageText, text));
            content.add_child(Box::new(
                Markdown::new(
                    format!("{header}{}", self.skill_block.content),
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
            // Collapsed：单行 `[skill] name (hint to expand)`。
            let line = format!(
                "{}{}{}",
                fg(ThemeColor::CustomMessageLabel, "\x1b[1m[skill]\x1b[22m "),
                fg(ThemeColor::CustomMessageText, &self.skill_block.name),
                fg(
                    ThemeColor::Dim,
                    &format!(
                        " ({} to expand)",
                        kiana_ui_kit::key_text("app.tools.expand")
                    ),
                ),
            );
            content.add_child(Box::new(Text::with_padding(line, 0, 0)));
        }

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

impl Component for SkillInvocationMessageComponent {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    /// 可变下转：设置页的 `set_output_pad` 与展开开关都靠它找到本类型。
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    /// 点击后 `dirty` 为真时先重建内容，再交由外壳渲染。
    ///
    /// `update_display` 会 `clear` 并重建 `bx` 的子项，因此这里的重建不会与缓存冲突：
    /// `BoxComponent::clear` 会丢弃渲染缓存。
    fn render(&mut self, width: usize) -> Vec<String> {
        if self.dirty.get() {
            self.update_display();
        }
        self.bx.render(width)
    }

    /// 转发给外壳；命中 `MouseRegion` 时由点击闭包翻转展开状态并置 `dirty`。
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.bx.handle_mouse(event)
    }

    /// TS 的 `override invalidate()`：先失效外壳，再重建内容（内容里的主题色可能已变）。
    fn invalidate(&mut self) {
        self.bx.invalidate();
        self.update_display();
    }

    /// TS：本组件没有键盘处理，焦点不会落在它上面。
    fn handles_input(&self) -> bool {
        false
    }

    /// 把外壳的子项（即包住内容的 `MouseRegion`）暴露给容器遍历/焦点逻辑。
    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        Some(&self.bx.children)
    }

    /// 可变版本，同上。
    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        Some(&mut self.bx.children)
    }
}
