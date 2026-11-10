//! components/custom-entry.ts。
//!
//! 扩展自定义会话条目的渲染壳：宿主负责转录间距，渲染器只提供内容。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/custom-entry.ts`。
//!
//! ## Public API for InteractiveMode
//!
//! - [`CustomEntryComponent::new`]（entry, renderer, outputPad）
//! - `has_content` / `set_expanded` / `set_output_pad`
//!
//! ## 被谁调用
//!
//! 宿主 `interactive_mode/render.rs` 的 `add_custom_entry_to_chat`：会话转录里出现扩展
//! 自定义条目时构造，`has_content()` 为 false 就跳过；展开开关经
//! `set_expanded_on_any`（`extension_ui/widgets.rs` 的 downcast 清单）到达这里。
//!
//! ## 设计原因
//!
//! 渲染器来自扩展（`EntryRenderer` 是扩展注册进 runner 的回调），扩展代码可能 panic。
//! 宿主必须保证一条坏条目只影响它自己：这里用 [`catch_unwind_silent`] 捕获 panic，转成
//! 一条红色错误框，而不是让 panic 从 `Container::render` 的调用栈里继续向外冒泡。
//! 该辅助函数同时会抑制本次 panic 的默认 stderr 输出（见 `markdown_transform` 的模块
//! 文档），避免破坏全屏 TUI。
//!
//! ## 执行流程
//!
//! 1. `new` 保存 entry / renderer / output_pad，并立即调用一次 `rebuild`；
//! 2. `rebuild` 先清空容器且把 `has_content` 置 false，再取当前主题调用渲染器；
//! 3. 渲染器返回 `Some(component)` → 追加 `Spacer` + 组件；返回 `None` → 保持空容器；
//!    `Err`（渲染器 panic）→ 追加一条错误框。主题未初始化也走 `Err` 分支，但该分支构造
//!    错误框时还要用 `bg()` 取主题，会在其 `expect` 处 panic——这条兜底只在主题已初始化
//!    时才能真正显示。
//! 4. `set_expanded` 只在值变化时重建；`set_output_pad` 总是重建（与 TS 相同）；
//!    `invalidate`（主题切换/强制重绘）也会重建，因此渲染器必须可重复调用。
//!
//! ## 注意事项
//!
//! * `output_pad` **不会**传给渲染器（`EntryRenderOptions` 只有 `expanded`），它只作为
//!   错误框的左内边距；自定义内容的缩进由渲染器自己决定。
//! * `has_content` 区分「渲染器返回 `None`」与「渲染成功」：调用方
//!   `render::add_custom_entry_to_chat` 在 false 时直接跳过该条目（不插入空行）；panic
//!   产生的错误框算内容，因此为 true。
//! * 每次重建都会 `clone` 一份 entry 交给渲染器，渲染器拿到的是快照。
//! * `fg` / `bg` 在主题未初始化时会 panic；错误框路径也依赖主题，测试必须先
//!   `init_theme`（见 `tests/components_messages.rs`）。

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use crate::components::markdown_transform::catch_unwind_silent;
use kiana_ext_api::types::{EntryRenderOptions, EntryRenderer};
use kiana_session_core::session_manager::CustomEntry;
use kiana_theme::theme_tokens::{ThemeBg, ThemeColor};
use kiana_tui::components::box_component::BoxComponent;
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

/// 从 panic 载荷提取 TS `String(error)` 的等价文本。
///
/// Rust 的 panic 载荷通常是 `String` 或 `&'static str`，这两种能还原文本；其它类型
/// （例如 `panic_any` 传入的自定义值）无法像 JS 对象那样格式化，统一显示
/// `Box<dyn Any>`。因此错误框只在载荷为 `String` / `&'static str` 时显示原始消息。
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else {
        "Box<dyn Any>".to_string()
    }
}

/// `class CustomEntryComponent extends Container`。
///
/// 用组合实现 TS 的 `extends Container`：所有子项放在内部的 [`Container`] 里，
/// `render` / `handle_mouse` / 子项遍历都转发过去，焦点与鼠标命中因此仍能到达渲染器
/// 返回的组件。宿主还会通过 `register_expandable` 登记它的 `set_expanded`，让全局的
/// 展开开关能作用到这条条目。
pub struct CustomEntryComponent {
    entry: CustomEntry,
    renderer: EntryRenderer,
    output_pad: usize,
    expanded: bool,
    has_content: bool,
    container: Container,
}

impl CustomEntryComponent {
    /// `constructor(entry, renderer, outputPad = 1)`。
    ///
    /// 构造时立即调用一次 `rebuild`，所以渲染器会在 `new` 返回前被调用一次（panic 会被
    /// 捕获，不会传播给调用方）。`output_pad` 只影响错误框的左内边距。
    pub fn new(entry: CustomEntry, renderer: EntryRenderer, output_pad: usize) -> Self {
        let mut component = CustomEntryComponent {
            entry,
            renderer,
            output_pad,
            expanded: false,
            has_content: false,
            container: Container::new(),
        };
        component.rebuild();
        component
    }

    /// `hasContent()`：渲染器是否返回了组件。
    ///
    /// 渲染器返回 `None` 时为 `false`（容器为空，调用方应跳过该条目）；返回组件或 panic
    /// 生成错误框时为 `true`。
    pub fn has_content(&self) -> bool {
        self.has_content
    }

    /// `setExpanded(expanded)`：只在变化时重建。
    ///
    /// 值相同时什么都不做，因此不会重复调用渲染器。
    pub fn set_expanded(&mut self, expanded: bool) {
        if self.expanded != expanded {
            self.expanded = expanded;
            self.rebuild();
        }
    }

    /// `setOutputPad(outputPad)`：**总是**重建。
    ///
    /// 与 `set_expanded` 不同，这里没有变化守卫：传相同值也会重建并重调渲染器。
    pub fn set_output_pad(&mut self, output_pad: usize) {
        self.output_pad = output_pad;
        self.rebuild();
    }

    /// 当前 output pad。
    ///
    /// 它只用作错误框的左内边距；`EntryRenderOptions` 不携带该值。
    pub fn output_pad(&self) -> usize {
        self.output_pad
    }

    fn rebuild(&mut self) {
        // 阶段 1：清空旧内容。返回 None 时容器必须保持全空（不能留下上一轮的 Spacer）。
        self.container.clear();
        self.has_content = false;

        // 阶段 2：取当前主题并调用渲染器；catch_unwind_silent 把扩展代码的 panic 隔离开。
        let options = EntryRenderOptions {
            expanded: self.expanded,
        };
        let rendered = match kiana_theme::THEME.current() {
            Ok(theme) => {
                let entry = self.entry.clone();
                catch_unwind_silent(AssertUnwindSafe(|| {
                    (self.renderer)(&entry, &options, &theme)
                }))
            }
            Err(error) => {
                // TS 里 `theme` 是 Proxy，未初始化会在渲染器首次读取时抛异常；异常被下面的
                // 错误分支同样捕获。这里没有可传的 `&Theme`，按渲染器失败处理。
                // 注意：紧接着构造错误框时 `bg()` 也要读主题；主题确实未初始化时会在它的
                // `expect` 处 panic，因此这条兜底只在主题已初始化时可用。
                Err(Box::new(error.to_string()) as Box<dyn std::any::Any + Send>)
            }
        };

        // 阶段 3：三种结果分别处理：Some → 直接使用；None → 无内容；Err → 错误框。
        let component = match rendered {
            Ok(Some(component)) => component,
            Ok(None) => return,
            Err(payload) => {
                let message = panic_text(payload.as_ref());
                let mut bx = BoxComponent::new(
                    self.output_pad,
                    1,
                    Some(Arc::new(|text: &str| bg(ThemeBg::CustomMessageBg, text))),
                );
                bx.add_child(Box::new(Text::with_padding(
                    fg(
                        ThemeColor::Error,
                        &format!("[{}] renderer failed: {message}", self.entry.custom_type),
                    ),
                    0,
                    0,
                )));
                Box::new(bx) as Box<dyn Component>
            }
        };

        // 阶段 4：只有走到这里才算有内容；上面的 `Ok(None)` 已经提前 return。
        self.has_content = true;
        self.container.add_child(Box::new(Spacer::new()));
        self.container.add_child(component);
    }
}

impl Component for CustomEntryComponent {
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
        // 先让旧子项失效，再整体重建：主题切换后渲染器需要拿到新主题重新渲染。
        self.container.invalidate();
        self.rebuild();
    }

    fn handles_input(&self) -> bool {
        false
    }

    /// 暴露内部容器子项，让焦点与鼠标遍历能穿过这层包装（对应 TS 的 `extends Container`）。
    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        Some(&self.container.children)
    }

    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        Some(&mut self.container.children)
    }
}
