//! components/custom-message.ts。
//!
//! 扩展自定义消息：优先用 `MessageRenderer`，否则渲染带 `customMessageBg` 背景的默认块。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/custom-message.ts`。
//!
//! ## Public API for InteractiveMode
//!
//! - [`CustomMessageComponent::new`]（message, renderer?, markdown theme, outputPad）
//! - `set_expanded` / `set_output_pad`
//!
//! ## 被谁调用
//!
//! 宿主 `interactive_mode/render.rs`：转录里出现 `CustomMessage` 时构造（渲染器取自
//! `runner.get_message_renderer(custom_type)`），展开开关经 `set_expanded_on_any`
//! 到达这里。与 [`crate::components::custom_entry::CustomEntryComponent`] 的差异：本组件总有内容（默认框兜底），
//! 没有「跳过条目」的分支。
//!
//! ## 设计原因
//!
//! 扩展可以注册 `MessageRenderer` 来自定义消息外观；没有注册、或渲染器选择不渲染时，
//! 宿主仍要显示一条可读的默认消息，否则该条目会在转录里静默消失。因此这里采用
//! 「渲染器优先、失败回退默认框」：返回 `None` 与 panic 都回退，对应 TS 的
//! `try/catch` + `if (component)`。
//!
//! ## 执行流程（`rebuild`）
//!
//! 1. 清空容器并重新加入 `Spacer(1)`（TS 里这个常量子项始终排在第一位）；
//! 2. 若有自定义渲染器：取主题、调用、`Some` 就直接挂上并返回（**不套默认 Box**）；
//! 3. 否则构造默认 Box：左内边距 = `output_pad`、上内边距 1、背景 `customMessageBg`；
//! 4. Box 内依次是手写粗体（`\x1b[1m`…`\x1b[22m`）的 `[customType]` 标签、`Spacer(1)`、
//!    Markdown 正文（正文颜色 `customMessageText`）。
//!
//! ## 注意事项
//!
//! * 自定义渲染器命中时既没有 `customMessageBg` 背景也没有内边距，只保留最前面的
//!   `Spacer(1)`；扩展需要自行对齐。
//! * 渲染器收到的选项是 `{ expanded, output_pad }`（比 `EntryRenderer` 多一个
//!   `output_pad`）。构造时渲染器恰好被调用一次，测试锁定了这次调用的完整参数
//!   （`tests/components_messages.rs` 的 `custom_message_*`）。
//! * `set_expanded` 与 `set_output_pad` 都有变化守卫；`invalidate` 没有，主题切换后会
//!   重调渲染器。
//! * 渲染器 panic 被 [`catch_unwind_silent`] 吞掉且不打印；与 `CustomEntryComponent`
//!   不同，这里没有错误框，直接回退默认渲染。
//! * 主题未初始化时自定义渲染器分支被跳过（`if let Ok(theme)` 不成立），随后默认框的
//!   `bg()` 同样依赖主题，会在其 `expect` 处 panic。因此本组件要求调用方先
//!   `init_theme`。
//! * 正文提取规则见 `custom_message_text`：非字符串且非数组（或数组里没有 text 块）得到
//!   空串，不会报错。

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use crate::components::markdown_transform::catch_unwind_silent;
use kiana_ext_api::types::{MessageRenderOptions, MessageRenderer};
use kiana_session_core::messages::CustomMessage;
use kiana_theme::theme_tokens::{ThemeBg, ThemeColor};
use kiana_tui::components::box_component::BoxComponent;
use kiana_tui::components::markdown::{DefaultTextStyle, Markdown, MarkdownTheme, StyleFn};
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::Text;
use kiana_tui::tui::{Component, Container};

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

/// `class CustomMessageComponent extends Container`。
///
/// 用组合实现 TS 的 `extends Container`：子项都在内部 [`Container`] 里，渲染、鼠标与
/// 子项遍历转发过去。宿主会通过 `register_expandable` 登记 `set_expanded`，让全局展开
/// 开关能作用到这条消息。
pub struct CustomMessageComponent {
    message: CustomMessage,
    custom_renderer: Option<MessageRenderer>,
    markdown_theme: MarkdownTheme,
    expanded: bool,
    output_pad: usize,
    container: Container,
}

impl CustomMessageComponent {
    /// `constructor(message, customRenderer?, markdownTheme = getMarkdownTheme(), outputPad = 1)`。
    ///
    /// 构造时立即 `rebuild`，因此渲染器（若提供）在 `new` 返回前被调用一次，收到的
    /// `output_pad` 就是此参数；`custom_renderer` 为 `None` 时只走默认渲染。
    pub fn new(
        message: CustomMessage,
        custom_renderer: Option<MessageRenderer>,
        markdown_theme: MarkdownTheme,
        output_pad: usize,
    ) -> Self {
        let mut component = CustomMessageComponent {
            message,
            custom_renderer,
            markdown_theme,
            expanded: false,
            output_pad,
            container: Container::new(),
        };
        component.container.add_child(Box::new(Spacer::new()));
        component.rebuild();
        component
    }

    /// `setExpanded(expanded)`：只在变化时重建。
    pub fn set_expanded(&mut self, expanded: bool) {
        if self.expanded != expanded {
            self.expanded = expanded;
            self.rebuild();
        }
    }

    /// `setOutputPad(outputPad)`：只在变化时重建。
    ///
    /// TS 同样有变化守卫；`CustomEntryComponent::set_output_pad` 没有。
    pub fn set_output_pad(&mut self, output_pad: usize) {
        if self.output_pad != output_pad {
            self.output_pad = output_pad;
            self.rebuild();
        }
    }

    /// 当前 output pad。
    ///
    /// 同时用于默认 Box 的左内边距和传给渲染器的选项。
    pub fn output_pad(&self) -> usize {
        self.output_pad
    }

    fn rebuild(&mut self) {
        // TS 用 removeChild(customComponent) + removeChild(box) 再按需加回；清空后重加
        // `Spacer(1)` 的可观察结果相同（Spacer 实例无外部引用）。
        self.container.clear();
        self.container.add_child(Box::new(Spacer::new()));

        // 阶段 1：自定义渲染器优先。Ok(None)（明确不渲染）与 Err（panic）都落到默认框，
        // 且不会向调用方传播 panic。
        if let Some(renderer) = self.custom_renderer.clone() {
            let options = MessageRenderOptions {
                expanded: self.expanded,
                output_pad: self.output_pad,
            };
            if let Ok(theme) = kiana_theme::THEME.current() {
                let message = self.message.clone();
                let rendered =
                    catch_unwind_silent(AssertUnwindSafe(|| renderer(&message, &options, &theme)));
                if let Ok(Some(component)) = rendered {
                    // 自定义渲染器自带样式：不套 Box。
                    self.container.add_child(component);
                    return;
                }
            }
        }

        // 阶段 2：默认渲染。Box 上内边距恒为 1，左内边距跟随 output_pad。
        let box_padding = self.output_pad;
        let mut bx = BoxComponent::new(
            box_padding,
            1,
            Some(Arc::new(|text: &str| bg(ThemeBg::CustomMessageBg, text))),
        );
        let label = fg(
            ThemeColor::CustomMessageLabel,
            &format!("\x1b[1m[{}]\x1b[22m", self.message.custom_type),
        );
        bx.add_child(Box::new(Text::with_padding(label, 0, 0)));
        bx.add_child(Box::new(Spacer::new()));

        // 阶段 3：正文。字符串原样；数组只取 text 块并用换行连接（见下方辅助函数）。
        let text = custom_message_text(&self.message.content);
        let color: StyleFn = Arc::new(|text: &str| fg(ThemeColor::CustomMessageText, text));
        bx.add_child(Box::new(
            Markdown::new(text, 0, 0, self.markdown_theme.clone()).with_default_text_style(
                DefaultTextStyle {
                    color: Some(color),
                    ..DefaultTextStyle::default()
                },
            ),
        ));
        self.container.add_child(Box::new(bx));
    }
}

/// `typeof content === "string" ? content : content.filter(c => c.type === "text").map(c => c.text).join("\n")`。
///
/// * `Value::String` → 原样返回（包括空串）；
/// * `Value::Array` → 只保留「元素是对象、`type == "text"`、`text` 是字符串」的块，按顺序
///   用 `\n` 连接；字段缺失或类型不符的块被跳过，不报错；
/// * 其它 JSON（对象、数字、null）→ 空串。
fn custom_message_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| {
                let object = block.as_object()?;
                if object.get("type")?.as_str()? != "text" {
                    return None;
                }
                Some(object.get("text")?.as_str()?.to_string())
            })
            .collect::<Vec<String>>()
            .join("\n"),
        _ => String::new(),
    }
}

impl Component for CustomMessageComponent {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn render(&mut self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn handle_mouse(
        &mut self,
        event: &kiana_tui::tui::TuiMouseEvent,
    ) -> Option<kiana_tui::tui::TuiMouseEventResult> {
        self.container.handle_mouse(event)
    }

    fn invalidate(&mut self) {
        self.container.invalidate();
        self.rebuild();
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        Some(&self.container.children)
    }

    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        Some(&mut self.container.children)
    }
}
