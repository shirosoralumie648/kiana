//! components/user-message.ts。
//!
//! 用户消息组件：单个带 `userMessageBg` 背景的 Markdown，加上 OSC 133 区域标记。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/user-message.ts`。
//!
//! ## Public API for InteractiveMode
//!
//! - [`UserMessageComponent::new`]（text, markdown theme, outputPad, transformers, messageId, timestamp）
//! - `set_output_pad` / `Component::{render,invalidate}`
//!
//! ## 在系统中的位置
//!
//! 由 `interactive_mode::render::add_message_to_chat` 构造并挂进聊天流（普通用户消息与
//! 技能调用块内嵌 `user_message` 两条路径都会走到这里）；设置面板修改内边距时经
//! `interactive_mode::commands::settings::set_output_pad` 向下转型调用
//! [`UserMessageComponent::set_output_pad`]。行为测试在 `tests/components_messages.rs`。
//!
//! ## 为什么需要这个组件
//!
//! 对话流里的用户消息必须与助手/工具消息在视觉上区分。这里把源文本交给一个 `Markdown`
//! 子组件渲染，并把默认前景/背景分别设为主题的 `userMessageText` / `userMessageBg`，
//! 因此整块（包括上下 padding 空行）都带用户消息底色。若直接把文本当普通组件输出，
//! Markdown 语法不会被解析，也无法随主题切换配色。
//!
//! ## 执行流程
//!
//! 1. [`UserMessageComponent::new`] 保存参数后立即调用内部 `rebuild`；
//! 2. `rebuild` 清空内部 `Container`，重新构造**唯一**一个 `Markdown`：水平内边距取
//!    `output_pad`、垂直内边距固定为 1，并挂上 `create_markdown_transform` 生成的回调
//!    （消息类型 `User`、非流式）；
//! 3. `render` 让 `Container` 渲染子项，再在首行行首插入 OSC 133 `A`、末行行首插入
//!    `B` + `C`；不新增行。
//!
//! ## 约束与注意事项
//!
//! - 主题必须在构造（`new` → `rebuild`）前初始化：文件内的 `fg`/`bg` 用 `expect` 读取
//!   全局主题，构造阶段计算默认样式前缀时即会调用，未初始化时构造就 panic
//!   （TS 则在 render 时才抛异常）。
//! - OSC 133 标记是零宽控制序列：不改变行数和可见宽度，支持 shell integration 的
//!   终端/备屏绘制层会在行首把它们剥掉（见 `kiana-tui` 的布局与 alt-screen 模块）。
//! - `invalidate` 只转发给子 `Markdown`；它清掉渲染缓存后，下一次 `render` 会重新执行
//!   transform，因此扩展 transformer 的外部状态变化不需要重建外层组件。
//! - `set_output_pad` 会重建子项，旧 `Markdown` 的渲染缓存随之丢弃；这是设置面板修改
//!   内边距后能立即生效的原因。

use std::sync::Arc;

use kiana_ext_api::types::{MarkdownMessageType, MarkdownTransformer};
use kiana_theme::theme_tokens::{ThemeBg, ThemeColor};
use kiana_tui::components::markdown::{
    DefaultTextStyle, Markdown, MarkdownOptions, MarkdownTheme, StyleFn,
};
use kiana_tui::tui::{Component, Container};

use super::markdown_transform::create_markdown_transform;

// OSC 133 区域标记：A 加到首行前，B、C 依次加到末行前。三者都是 OSC（操作系统命令）
// 控制序列，以 BEL 结尾；不含可见字符，因此不参与宽度计算。常量是当前稳定契约，
// 改动需同步测试。
const OSC133_ZONE_START: &str = "\x1b]133;A\x07";
const OSC133_ZONE_END: &str = "\x1b]133;B\x07";
const OSC133_ZONE_FINAL: &str = "\x1b]133;C\x07";

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

/// `class UserMessageComponent extends Container`。
///
/// 用户消息的渲染外壳。内部只持有一个 `Container`，渲染、鼠标路由与子项遍历都委托给它；
/// 这样外部可以按普通容器对待本组件（例如宿主通过 `container_children` 穿过多层包装），
/// 而 `render` 仍能在整块首尾插入 OSC 133 标记。
///
/// 字段都是私有的：`text` 是 Markdown 源文本，`output_pad` 是水平内边距，
/// `message_id` / `timestamp` 只用于构造 transformer 的上下文（历史消息有值，live 消息为
/// `None`）。构造后修改这些状态只能通过 [`UserMessageComponent::set_output_pad`]。
pub struct UserMessageComponent {
    text: String,
    markdown_theme: MarkdownTheme,
    output_pad: usize,
    markdown_transformers: Vec<MarkdownTransformer>,
    message_id: Option<String>,
    timestamp: Option<String>,
    container: Container,
}

impl UserMessageComponent {
    /// `constructor(text, markdownTheme = getMarkdownTheme(), outputPad = 1,
    /// markdownTransformers = [], messageId?, timestamp?)`。
    ///
    /// 构造后立即调用内部 `rebuild` 生成子项；`rebuild` 会读取全局主题
    /// （`with_default_text_style` 计算默认样式前缀时探测 color 闭包），
    /// 主题未初始化时构造即 panic。
    ///
    /// 参数：
    /// - `text`：Markdown 源文本；
    /// - `markdown_theme`：Markdown 各元素（标题、代码等）的样式函数集合；
    /// - `output_pad`：水平内边距，直接作为 `Markdown` 的 `padding_x`；
    /// - `markdown_transformers`：扩展提供的变换链，按 Vec 顺序执行；
    /// - `message_id` / `timestamp`：透传给 transform 上下文，`None` 表示尚未落盘的
    ///   live 消息。
    ///
    /// Rust 没有默认参数，TS 的默认值（`output_pad = 1`、空列表等）由调用方显式传入。
    pub fn new(
        text: impl Into<String>,
        markdown_theme: MarkdownTheme,
        output_pad: usize,
        markdown_transformers: Vec<MarkdownTransformer>,
        message_id: Option<String>,
        timestamp: Option<String>,
    ) -> Self {
        let mut component = UserMessageComponent {
            text: text.into(),
            markdown_theme,
            output_pad,
            markdown_transformers,
            message_id,
            timestamp,
            container: Container::new(),
        };
        component.rebuild();
        component
    }

    /// `setOutputPad(padding)`：存值并重建。
    ///
    /// 重建而不是原地修改：padding 是 `Markdown` 的构造参数，且旧子项的渲染缓存要一并
    /// 丢弃。设置面板会通过 `dyn Component` 向下转型调用本方法，因此外层组件实例保持
    /// 不变，宿主持有的引用/焦点不会失效。
    pub fn set_output_pad(&mut self, padding: usize) {
        self.output_pad = padding;
        self.rebuild();
    }

    /// 当前 output pad。
    pub fn output_pad(&self) -> usize {
        self.output_pad
    }

    /// 源文本。
    pub fn text(&self) -> &str {
        &self.text
    }

    fn rebuild(&mut self) {
        self.container.clear();
        // 每次重建都丢弃旧的 Markdown 与它的渲染缓存；transform 闭包按当前字段重新生成。
        // 前景/背景用两个独立的 StyleFn：Markdown 的 DefaultTextStyle 会分别在文字片段和
        // 整行填充阶段调用它们，主题 token 在调用时才读取，因此主题切换后重新渲染，
        // 正文前景与整行背景即可换色；但 Markdown 的默认样式前缀在构造时计算一次
        // （`kiana-tui` 的 markdown 模块构造后不再修改主题，`invalidate` 只清渲染缓存
        // 不重算前缀），主题切换后需重建子项（如 `set_output_pad` 路径）才能同步。
        // 如果外面再包一层背景 Box，会与 Markdown 自身的逐行填充重复，产生相同的输出。
        let color: StyleFn = Arc::new(|content: &str| fg(ThemeColor::UserMessageText, content));
        let bg_color: StyleFn = Arc::new(|content: &str| bg(ThemeBg::UserMessageBg, content));
        // 用户消息始终非流式（is_streaming = false）；message_id/timestamp 会出现在每个
        // transformer 的上下文里，用于让扩展区分历史消息与 live 消息。
        let transform = create_markdown_transform(
            MarkdownMessageType::User,
            false,
            self.markdown_transformers.clone(),
            self.message_id.clone(),
            self.timestamp.clone(),
        );
        let markdown = Markdown::new(
            self.text.clone(),
            self.output_pad,
            1,
            self.markdown_theme.clone(),
        )
        .with_default_text_style(DefaultTextStyle {
            color: Some(color),
            bg_color: Some(bg_color),
            ..DefaultTextStyle::default()
        })
        // 保留源列表 marker（`1)`/`*` 等）与反斜杠转义，避免用户原文在渲染时被归一化。
        .with_options(MarkdownOptions {
            preserve_ordered_list_markers: true,
            preserve_backslash_escapes: true,
            transform: Some(transform),
            ..MarkdownOptions::default()
        });
        self.container.add_child(Box::new(markdown));
    }
}

impl Component for UserMessageComponent {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = self.container.render(width);
        // 空内容不插入标记：没有行可挂，返回空 Vec 让上层把它当作零行组件。
        if lines.is_empty() {
            return lines;
        }
        // 标记是零宽序列，直接拼接即可，不会改变行数或可见宽度（测试锁定高度不变）。
        // 首行加 A；末行同时加 B 和 C，且 B 在 C 之前。
        lines[0] = format!("{OSC133_ZONE_START}{}", lines[0]);
        let last = lines.len() - 1;
        // 边界：如果子项恰好只渲染出一行，首行与末行是同一行，结果会成为
        // `B C A <text>`（先在本行前加 A，再把 B/C 加到同一行行首）。TS 行为相同；
        // 正常路径下 Markdown 的上下 padding 会产出多行，不会走到这个形状。
        lines[last] = format!("{OSC133_ZONE_END}{OSC133_ZONE_FINAL}{}", lines[last]);
        lines
    }

    fn handle_mouse(
        &mut self,
        event: &kiana_tui::tui::TuiMouseEvent,
    ) -> Option<kiana_tui::tui::TuiMouseEventResult> {
        // 按子项行高换算坐标后把事件交给容器（容器负责路由到命中的子组件）。
        self.container.handle_mouse(event)
    }

    /// TS 里 `UserMessageComponent` 没有覆写 `invalidate()`，继承 `Container.invalidate()`
    /// （把失效转发给子 `Markdown`，由它重新执行 transform）。
    ///
    /// 因此这里不能顺手调用 `rebuild`：重建会替换子组件并丢弃缓存，而 TS 语义只是让
    /// 子 `Markdown` 的渲染缓存失效。测试「invalidate 后 transformer 重新执行」锁定的就是
    /// 这条转发路径。
    fn invalidate(&mut self) {
        self.container.invalidate();
    }

    fn handles_input(&self) -> bool {
        // 纯展示组件：TS 没有 handleInput，宿主不应把键盘输入或焦点路由到这里。
        false
    }

    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        // 暴露内部容器的子项，让宿主的焦点/挂载遍历能穿过这层包装（见 Component 的
        // container_children 契约）。
        Some(&self.container.children)
    }

    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        // 与不可变版本配对，供需要写 `focused` 标志的遍历使用。
        Some(&mut self.container.children)
    }
}
