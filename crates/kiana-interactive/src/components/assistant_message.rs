//! components/assistant-message.ts。
//!
//! 完整的 assistant 消息组件：文本 / thinking 块（可折叠、按 run 记忆可见性）、
//! 长度截断与 aborted/error 提示，以及 OSC 133 区域标记。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/assistant-message.ts`。
//!
//! ## 调用方
//!
//! 本组件有两个生产构造路径，对应 assistant 消息的生命周期：
//! 1. **流式开始**：`interactive_mode/events.rs` 的 `MessageStart` 分支构造组件并挂进
//!    聊天区，记为「当前流式组件」；后续 `MessageUpdate` 就地调
//!    [`AssistantMessageComponent::update_content_streaming`]，`MessageEnd` 以
//!    `is_streaming = false` 做最终一次刷新（Aborted 时调用方会先把错误文案写进
//!    `error_message`，本组件照原样渲染）。
//! 2. **历史回放/一次性消息**：`interactive_mode/render.rs` 的 `add_message_to_chat`
//!    `assistant` 分支，构造后即完整渲染，不再流式更新。
//!
//! 两条路径构造后都会把句柄登记进扩展 API 的活动组件注册表
//! （`interactive_mode/extension_ui/widgets.rs`，对应 TS 的
//! `instanceof AssistantMessageComponent`）：`/thinking` 等命令改隐藏开关或折叠标签时，
//! 经注册表对所有存活组件广播调用 `set_hide_thinking_block` /
//! `set_hidden_thinking_label`。注册表不是第三条构造路径，只是两条生产构造路径的
//! 共同副作用。
//!
//! `kiana-tui` 的备用屏（`alt_screen.rs` 的 `scroll_to_prompt`）与布局层
//! （`layout.rs` 的 `strip_osc133_zone_prefix`）消费 [`Component::render`] 输出里的
//! OSC 133 标记，用于「滚动到提示行」与剥掉行首标记后再绘制。
//!
//! ## Public API for InteractiveMode
//!
//! - [`AssistantMessageComponent::new`]（TS 8 参构造）/ `Default`
//! - `update_content` / `update_content_streaming` / `set_hide_thinking_block`
//!   / `set_hidden_thinking_label` / `set_output_pad`
//! - `Component`：`render` / `handle_mouse` / `invalidate`

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use kiana_ai::types::{AssistantContentBlock, AssistantMessage, StopReason};
use kiana_ext_api::types::{MarkdownMessageType, MarkdownTransformer};
use kiana_theme::theme_tokens::ThemeColor;
use kiana_tui::components::markdown::{
    DefaultTextStyle, Markdown, MarkdownOptions, MarkdownTheme, StyleFn,
};
use kiana_tui::components::mouse_region::MouseRegion;
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::Text;
use kiana_tui::tui::{
    Component, Container, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType,
};

use super::markdown_transform::create_markdown_transform;

const OSC133_ZONE_START: &str = "\x1b]133;A\x07";
const OSC133_ZONE_END: &str = "\x1b]133;B\x07";
const OSC133_ZONE_FINAL: &str = "\x1b]133;C\x07";
// A 加到首行前，B、C 依次加到末行前：A 标记一段提示（prompt）的开始，B/C 标记结束与
// 输出落定。三者都是 OSC 控制序列、以 BEL 结尾，不含可见字符，因此不参与宽度计算。
// 常量是稳定契约（ANSI 控制序列，逐字节不可变）；消费方见上面的模块文档（alt_screen 的滚动定位与 layout 的剥除）。

/// 生产路径的主题着色（`theme` 全局代理；未初始化时 TS 会抛异常）。
fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

fn italic(text: &str) -> String {
    kiana_theme::THEME
        .italic(text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// `class AssistantMessageComponent extends Container`。
///
/// 组件自身只承载一个内部 `Container`（TS 的 `contentContainer`），`updateContent` 重建它。
pub struct AssistantMessageComponent {
    content_container: Container,
    hide_thinking_block: bool,
    markdown_theme: MarkdownTheme,
    hidden_thinking_label: String,
    output_pad: usize,
    markdown_transformers: Vec<MarkdownTransformer>,
    last_message: Option<AssistantMessage>,
    has_tool_calls: bool,
    is_streaming: bool,
    message_id: Option<String>,
    timestamp: Option<String>,
    /// TS `thinkingVisibilityOverrides: Map<number, boolean>`。
    ///
    /// 点击处理器（`MouseRegion` 闭包）无法借用 `&mut self`，因此与组件共享这份状态；
    /// 处理器只写覆盖值并置脏标记，下一次 `render` 重放 `updateContent`（TS 在点击回调里
    /// 直接重建，可观察结果相同）。
    thinking_visibility_overrides: Rc<RefCell<BTreeMap<usize, bool>>>,
    dirty: Rc<Cell<bool>>,
}

impl Default for AssistantMessageComponent {
    fn default() -> Self {
        AssistantMessageComponent::new(
            None,
            false,
            kiana_theme::get_markdown_theme(),
            "Thinking...",
            1,
            Vec::new(),
            None,
            None,
        )
    }
}

impl AssistantMessageComponent {
    /// `constructor(message?, hideThinkingBlock = false, markdownTheme = getMarkdownTheme(),
    /// hiddenThinkingLabel = "Thinking...", outputPad = 1, markdownTransformers = [],
    /// messageId?, timestamp?)`。
    #[allow(
        clippy::too_many_arguments,
        reason = "TS 构造函数就是 8 个参数；拆成 builder 会改变调用顺序与默认值"
    )]
    pub fn new(
        message: Option<AssistantMessage>,
        hide_thinking_block: bool,
        markdown_theme: MarkdownTheme,
        hidden_thinking_label: impl Into<String>,
        output_pad: usize,
        markdown_transformers: Vec<MarkdownTransformer>,
        message_id: Option<String>,
        timestamp: Option<String>,
    ) -> Self {
        let mut component = AssistantMessageComponent {
            content_container: Container::new(),
            hide_thinking_block,
            markdown_theme,
            hidden_thinking_label: hidden_thinking_label.into(),
            output_pad,
            markdown_transformers,
            last_message: None,
            has_tool_calls: false,
            is_streaming: false,
            message_id,
            timestamp,
            thinking_visibility_overrides: Rc::new(RefCell::new(BTreeMap::new())),
            dirty: Rc::new(Cell::new(false)),
        };
        if let Some(message) = message {
            component.update_content(&message);
        }
        component
    }

    /// `setHideThinkingBlock(hide)`：无条件清空点击覆盖。
    pub fn set_hide_thinking_block(&mut self, hide: bool) {
        self.hide_thinking_block = hide;
        self.thinking_visibility_overrides.borrow_mut().clear();
        if let Some(message) = self.last_message.clone() {
            self.update_content(&message);
        }
    }

    /// `setHiddenThinkingLabel(label)`。
    pub fn set_hidden_thinking_label(&mut self, label: impl Into<String>) {
        self.hidden_thinking_label = label.into();
        if let Some(message) = self.last_message.clone() {
            self.update_content(&message);
        }
    }

    /// `setOutputPad(padding)`。
    pub fn set_output_pad(&mut self, padding: usize) {
        self.output_pad = padding;
        if let Some(message) = self.last_message.clone() {
            self.update_content(&message);
        }
    }

    /// 当前 output pad。
    pub fn output_pad(&self) -> usize {
        self.output_pad
    }

    /// `updateContent(message, isStreaming = this.isStreaming)`。
    pub fn update_content(&mut self, message: &AssistantMessage) {
        let is_streaming = self.is_streaming;
        self.update_content_streaming(message, is_streaming);
    }

    /// 带显式流式状态的 [`Self::update_content`]。
    pub fn update_content_streaming(&mut self, message: &AssistantMessage, is_streaming: bool) {
        self.last_message = Some(message.clone());
        self.is_streaming = is_streaming;
        self.dirty.set(false);

        self.content_container.clear();

        fn is_visible(block: &AssistantContentBlock) -> bool {
            match block {
                AssistantContentBlock::Text(text) => !text.text.trim().is_empty(),
                AssistantContentBlock::Thinking(thinking) => !thinking.thinking.trim().is_empty(),
                AssistantContentBlock::ToolCall(_) => false,
            }
        }
        if message.content.iter().any(is_visible) {
            self.content_container.add_child(Box::new(Spacer::new()));
        }

        let mut thinking_run_index = 0usize;
        let mut i = 0usize;
        while i < message.content.len() {
            match &message.content[i] {
                AssistantContentBlock::Text(text) => {
                    if !text.text.trim().is_empty() {
                        let transform = create_markdown_transform(
                            MarkdownMessageType::Assistant,
                            self.is_streaming,
                            self.markdown_transformers.clone(),
                            self.message_id.clone(),
                            self.timestamp.clone(),
                        );
                        let markdown = Markdown::new(
                            text.text.trim(),
                            self.output_pad,
                            0,
                            self.markdown_theme.clone(),
                        )
                        .with_options(MarkdownOptions {
                            transform: Some(transform),
                            ..MarkdownOptions::default()
                        });
                        self.content_container.add_child(Box::new(markdown));
                    }
                    i += 1;
                }
                AssistantContentBlock::Thinking(_) => {
                    // 连续的 Thinking 块合并成一次「run」：收集到第一个非 thinking 块为止，
                    // 空白 thinking 只跳过不产生 run。点击覆盖以 run 的序号（在本次重建中
                    // 是第几个 run，不是块下标）为键，跨重建保存在共享的
                    // `thinking_visibility_overrides` 里。
                    let mut thinking_blocks: Vec<String> = Vec::new();
                    while i < message.content.len() {
                        let AssistantContentBlock::Thinking(thinking) = &message.content[i] else {
                            break;
                        };
                        let thinking = thinking.thinking.trim();
                        if !thinking.is_empty() {
                            thinking_blocks.push(thinking.to_string());
                        }
                        i += 1;
                    }
                    // TS 在收集后 `i--`，外层 `for` 的 `i++` 把它还原成「第一个非 thinking 块」。
                    if thinking_blocks.is_empty() {
                        continue;
                    }

                    let has_visible_content_after = message.content[i..].iter().any(is_visible);

                    // 可见性按优先级合并：点击覆盖（若有）优先于全局 hide_thinking_block。
                    // 闭包捕获的是构造时的 hidden 值——重建后每个 run 会拿到新闭包。
                    let run_index = thinking_run_index;
                    thinking_run_index += 1;
                    let hidden = self
                        .thinking_visibility_overrides
                        .borrow()
                        .get(&run_index)
                        .copied()
                        .unwrap_or(self.hide_thinking_block);

                    let thinking_component: Box<dyn Component> = if hidden {
                        Box::new(Text::with_padding(
                            italic(&fg(ThemeColor::ThinkingText, &self.hidden_thinking_label)),
                            self.output_pad,
                            0,
                        ))
                    } else {
                        let color: StyleFn =
                            std::sync::Arc::new(|text: &str| fg(ThemeColor::ThinkingText, text));
                        let transform = create_markdown_transform(
                            MarkdownMessageType::AssistantThinking,
                            self.is_streaming,
                            self.markdown_transformers.clone(),
                            self.message_id.clone(),
                            self.timestamp.clone(),
                        );
                        Box::new(
                            Markdown::new(
                                thinking_blocks.join("\n\n"),
                                self.output_pad,
                                0,
                                self.markdown_theme.clone(),
                            )
                            .with_default_text_style(DefaultTextStyle {
                                color: Some(color),
                                italic: true,
                                ..DefaultTextStyle::default()
                            })
                            .with_options(MarkdownOptions {
                                transform: Some(transform),
                                ..MarkdownOptions::default()
                            }),
                        )
                    };

                    let overrides = Rc::clone(&self.thinking_visibility_overrides);
                    let dirty = Rc::clone(&self.dirty);
                    // 点击闭包只写覆盖值 + 置脏，不直接重建容器（重建要 &mut self，
                    // 闭包拿不到）；下一次 render 的脏检查会以新覆盖重放 update_content。
                    // 「写 !hidden」是按闭包捕获的旧可见性取反，覆盖值因此总是与本次
                    // 渲染的 run 状态成对。
                    let region = MouseRegion::new(
                        thinking_component,
                        Box::new(move |event: &TuiMouseEvent| {
                            if event.event_type != TuiMouseEventType::Click
                                || event.button != TuiMouseButton::Left
                            {
                                return None;
                            }
                            overrides.borrow_mut().insert(run_index, !hidden);
                            dirty.set(true);
                            Some(TuiMouseEventResult {
                                handled: Some(true),
                                ..TuiMouseEventResult::default()
                            })
                        }),
                    );
                    self.content_container.add_child(Box::new(region));
                    if has_visible_content_after {
                        self.content_container.add_child(Box::new(Spacer::new()));
                    }
                }
                AssistantContentBlock::ToolCall(_) => {
                    // 工具调用由 ToolExecutionComponent 单独渲染（events.rs 的 MessageUpdate
                    // 分支按 call.id 维护独立组件），这里跳过、不占位置。
                    i += 1;
                }
            }
        }

        // 消息收尾的三个状态行（互斥，都带前置空行）：
        // - Length：截断提示（与 Aborted 的自定义错误分开，用中性措辞）；
        // - Aborted / Error：仅在**没有**工具调用时显示（TS 同条件）；有错误文案用原文，
        //   Aborted 下 "Request was aborted" 视为占位符、换回默认文案，避免把内部
        //   哨兵字符串暴露给用户。
        let has_tool_calls = message
            .content
            .iter()
            .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)));
        self.has_tool_calls = has_tool_calls;
        if message.stop_reason == StopReason::Length {
            self.content_container.add_child(Box::new(Spacer::new()));
            self.content_container
                .add_child(Box::new(Text::with_padding(
                    fg(
                        ThemeColor::Error,
                        "Response was truncated before completion.",
                    ),
                    self.output_pad,
                    0,
                )));
        } else if !has_tool_calls {
            if message.stop_reason == StopReason::Aborted {
                let abort_message = match message.error_message.as_deref() {
                    Some(error) if !error.is_empty() && error != "Request was aborted" => error,
                    _ => "Operation aborted",
                };
                self.content_container.add_child(Box::new(Spacer::new()));
                self.content_container
                    .add_child(Box::new(Text::with_padding(
                        fg(ThemeColor::Error, abort_message),
                        self.output_pad,
                        0,
                    )));
            } else if message.stop_reason == StopReason::Error {
                let error_message = match message.error_message.as_deref() {
                    Some(error) if !error.is_empty() => error,
                    _ => "Unknown error",
                };
                self.content_container.add_child(Box::new(Spacer::new()));
                self.content_container
                    .add_child(Box::new(Text::with_padding(
                        fg(ThemeColor::Error, &format!("Error: {error_message}")),
                        self.output_pad,
                        0,
                    )));
            }
        }
    }

    /// 最近一次渲染的消息（测试/宿主查询用）。
    pub fn last_message(&self) -> Option<&AssistantMessage> {
        self.last_message.as_ref()
    }
}

impl Component for AssistantMessageComponent {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn render(&mut self, width: usize) -> Vec<String> {
        // 点击处理器无法可变借用组件，只能置脏标记；真正的重建延迟到这里——
        // 每次渲染前最多重放一次 update_content（测试 collapses_individual_thinking_runs_
        // when_clicked 依赖这个「点击后下一次 render 生效」的时序）。
        if self.dirty.get()
            && let Some(message) = self.last_message.clone()
        {
            self.update_content(&message);
        }
        let mut lines = self.content_container.render(width);
        // 工具调用消息不插标记：工具结果另有呈现方式，TS 也跳过（测试
        // does_not_add_osc_133_zone_markers_when_assistant_message_contains_tool_calls）。
        // 空内容同样返回原样——没有行可挂标记。
        if self.has_tool_calls || lines.is_empty() {
            return lines;
        }
        lines[0] = format!("{OSC133_ZONE_START}{}", lines[0]);
        let last = lines.len() - 1;
        lines[last] = format!("{OSC133_ZONE_END}{OSC133_ZONE_FINAL}{}", lines[last]);
        lines
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.content_container.handle_mouse(event)
    }

    /// `invalidate()`：容器缓存失效后整体重放 `updateContent`。
    ///
    /// 与 `render` 的脏检查不同（只重放一次、且需要 `dirty` 置位），`invalidate` 是宽度
    /// 环境变化时的全量重建，不依赖脏标记。
    fn invalidate(&mut self) {
        self.content_container.invalidate();
        if let Some(message) = self.last_message.clone() {
            self.update_content(&message);
        }
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        Some(&self.content_container.children)
    }

    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        Some(&mut self.content_container.children)
    }
}
