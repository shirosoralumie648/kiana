//! components/bash-execution.ts。
//!
//! 内联 `!` / `!!` 命令执行块：命令头、流式输出、折叠预览（按渲染时宽度重算）与状态行。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/bash-execution.ts`。
//!
//! ## Public API for InteractiveMode
//!
//! - [`BashExecutionComponent::new`]（command, `Option<RequestRenderCallback>`, excludeFromContext, outputPad）
//! - `append_output` / `set_complete` / `set_expanded` / `set_output_pad`
//! - `get_output` / `get_command`
//! - `set_timer`：安装 loader 的动画节拍器（TS 的 `Loader.setInterval`；没有宿主定时器时
//!   只会在其它事件触发的渲染里更新帧）
//!
//! # 职责与边界
//!
//! 交互模式在用户提交 `!`（输出进入模型上下文）或 `!!`（`exclude_from_context`，输出不进入
//! 上下文）命令时，用本组件在会话流里就地展示这次执行。组件只负责**渲染与状态展示**：
//! 命令由 `kiana-session` / `kiana-tools` 执行，宿主在流式回调里调用
//! [`BashExecutionComponent::append_output`] 推入 chunk，结束后调用
//! [`BashExecutionComponent::set_complete`] 收尾。实时执行的调用方见
//! [`crate::interactive_mode::commands::bash_compact`]；历史消息重放的调用方见
//! [`crate::interactive_mode::render`]。
//!
//! # 状态机
//!
//! 状态只有四种（私有枚举 `BashStatus`），只在构造与 `set_complete` 时变化：
//!
//! 1. 构造后为 `Running`：渲染命令头与 loader 动画，不渲染状态行；
//! 2. `set_complete` 时 `cancelled == true` → `Cancelled`（优先于退出码）；
//! 3. 否则退出码存在且非 0 → `Error`；
//! 4. 其余情况（`Some(0)` 或 `None`）→ `Complete`。注意“没有退出码”也归为完成。
//!
//! `set_complete` 会停止 loader 动画；之后组件不再发生状态迁移。命令运行期间重复调用
//! `append_output` 不会改变状态，只累积输出。
//!
//! # 一次渲染的流程
//!
//! `update_display` 重建内容容器，`render` 再按固定顺序拼装：
//!
//! 1. 把已收集的输出用 `"\n"` 拼回完整文本，先用与 bash 工具相同的 `truncate_tail`
//!    （[`DEFAULT_MAX_LINES`] 行 / [`DEFAULT_MAX_BYTES`] 字节）做上下文上限截断；
//! 2. 折叠时取最后 `PREVIEW_LINES`（20）条**逻辑行**交给 `CollapsedPreview`，后者再按
//!    **渲染宽度**做可视行截断（`Keep::End`），所以窗口缩放后会重新换行；
//! 3. 头部、正文、状态行都以 `output_pad` 作为左右留白加入内容容器；
//! 4. `render` 依次输出 spacer（默认 1 个空行）、上边框、内容容器、运行中的 loader
//!    （loader 自身还会再前置一个空行）、下边框。
//!
//! # 注意事项
//!
//! - 折叠提示里的 `hidden_line_count` 按**逻辑行**计数，而 `CollapsedPreview` 按**可视行**
//!   截取；长行被折行时，提示的 “... N more lines” 与实际隐藏的可视行数可能不同。
//! - 预览缓存键是渲染宽度：同宽度直接复用，宽度变化才重新截断；
//!   `Component::invalidate` 会清内容容器与 loader 的缓存后重建内容。
//! - `fg` / `bold` 在全局主题未初始化时 panic（对应 TS 在同样条件下抛异常），组件不捕获。
//! - `get_output` 返回的是 `append_output` 累积的原文（已剥 ANSI、已归一换行），不是
//!   渲染时的截断结果。

use kiana_core::ansi::strip_ansi;
use kiana_theme::theme_tokens::ThemeColor;
use kiana_tools::truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncationOptions, TruncationResult, truncate_tail,
};
use kiana_tui::components::loader::{Loader, RequestRenderCallback};
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::StyleFn;
use kiana_tui::components::text_component::Text;
use kiana_tui::text::visible_width;
use kiana_tui::tui::{Component, Container, Timer};
use kiana_ui_kit::visual_truncate::{Keep, truncate_to_visual_lines};
use kiana_ui_kit::{DynamicBorder, key_hint, key_text};

/// 未展开时的预览行数（与工具执行行为一致）。
const PREVIEW_LINES: usize = 20;

/// 一次内联 bash 执行的内部状态；只由构造与 [`BashExecutionComponent::set_complete`] 赋值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BashStatus {
    Running,
    Complete,
    Cancelled,
    Error,
}

fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

fn bold(text: &str) -> String {
    kiana_theme::THEME
        .bold(text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// 折叠预览的匿名组件（TS `contentContainer.addChild({ render, invalidate })`）。
///
/// 缓存 `(width, lines)`：宽度不变时复用；`invalidate()` 清缓存。
struct CollapsedPreview {
    text: String,
    padding_x: usize,
    cache: Option<(usize, Vec<String>)>,
}

impl CollapsedPreview {
    fn new(text: String, padding_x: usize) -> Self {
        CollapsedPreview {
            text,
            padding_x,
            cache: None,
        }
    }
}

impl Component for CollapsedPreview {
    fn render(&mut self, width: usize) -> Vec<String> {
        if let Some((cached_width, lines)) = &self.cache
            && *cached_width == width
        {
            return lines.clone();
        }
        // 缓存 key 只含宽度：同宽度下重复 render 直接复用上次的可视行；
        // 宽度变化（终端缩放）才重新换行并按 Keep::End 截取尾部。
        let result =
            truncate_to_visual_lines(&self.text, PREVIEW_LINES, width, self.padding_x, Keep::End);
        self.cache = Some((width, result.visual_lines.clone()));
        result.visual_lines
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {
        self.cache = None;
    }
}

/// `class BashExecutionComponent extends Container`。
///
/// 字段分三类：命令与原始输出（`command` / `output_lines`）、执行状态与截断信息
/// （`status` / `exit_code` / `truncation_result` / `full_output_path`）、渲染树
/// （`content_container` 与上下边框）。`output_lines` 保存完整输出，显示用的
/// 上限截断在每次 `update_display` 时重新计算，因此改 `output_pad` 或渲染宽度
/// 都不需要改动原始数据。
pub struct BashExecutionComponent {
    command: String,
    output_lines: Vec<String>,
    status: BashStatus,
    exit_code: Option<i32>,
    loader: Loader,
    truncation_result: Option<TruncationResult>,
    full_output_path: Option<String>,
    expanded: bool,
    content_container: Container,
    /// `dim` 标记 `!!` 命令（输出不进模型上下文）。
    color_key: ThemeColor,
    output_pad: usize,
    spacer: Spacer,
    top_border: DynamicBorder,
    bottom_border: DynamicBorder,
}

impl BashExecutionComponent {
    /// `constructor(command, ui, excludeFromContext = false, outputPad = 1)`。
    ///
    /// `request_render` 交给 loader，用于动画节拍与状态变化后请求重绘；
    /// `exclude_from_context` 只决定配色（`!!` 用 `Dim`，`!` 用 `BashMode`）；
    /// `output_pad` 是正文与状态行的左右留白列数。构造末尾先渲染一次（此时没有任何输出）。
    pub fn new(
        command: impl Into<String>,
        request_render: Option<RequestRenderCallback>,
        exclude_from_context: bool,
        output_pad: usize,
    ) -> Self {
        let command = command.into();
        let color_key = if exclude_from_context {
            ThemeColor::Dim
        } else {
            ThemeColor::BashMode
        };
        let border_color: StyleFn = std::sync::Arc::new(move |text: &str| fg(color_key, text));
        let loader = Loader::new(
            request_render,
            std::sync::Arc::new(move |spinner: &str| fg(color_key, spinner)),
            std::sync::Arc::new(|text: &str| fg(ThemeColor::Muted, text)),
            &format!("Running... ({} to cancel)", key_text("tui.select.cancel")),
            None,
        );
        let mut component = BashExecutionComponent {
            command,
            output_lines: Vec::new(),
            status: BashStatus::Running,
            exit_code: None,
            loader,
            truncation_result: None,
            full_output_path: None,
            expanded: false,
            content_container: Container::new(),
            color_key,
            output_pad,
            spacer: Spacer::new(),
            top_border: DynamicBorder::new(std::sync::Arc::clone(&border_color)),
            bottom_border: DynamicBorder::new(border_color),
        };
        component.update_display();
        component
    }

    /// `setExpanded(expanded)`。
    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
        self.update_display();
    }

    /// `setOutputPad(outputPad)`。
    pub fn set_output_pad(&mut self, output_pad: usize) {
        self.output_pad = output_pad;
        self.update_display();
    }

    /// 当前 output pad。
    pub fn output_pad(&self) -> usize {
        self.output_pad
    }

    /// `appendOutput(chunk)`：剥 ANSI、归一换行、续接最后一行，然后重建显示。
    pub fn append_output(&mut self, chunk: &str) {
        let clean = strip_ansi(chunk).replace("\r\n", "\n").replace('\r', "\n");
        let new_lines: Vec<&str> = clean.split('\n').collect();
        if !self.output_lines.is_empty() {
            // 首段接到当前最后一行（流式 chunk 可能在同一行内断开）。
            self.output_lines
                .last_mut()
                .expect("checked non-empty")
                .push_str(new_lines.first().copied().unwrap_or_default());
            self.output_lines
                .extend(new_lines.iter().skip(1).map(|line| (*line).to_string()));
        } else {
            self.output_lines
                .extend(new_lines.iter().map(|line| (*line).to_string()));
        }
        self.update_display();
    }

    /// `setComplete(exitCode, cancelled, truncationResult?, fullOutputPath?)`。
    ///
    /// 状态优先级：`cancelled` 最高，其次非 0 退出码为 `Error`，其余（含 `exitCode == None`）
    /// 为 `Complete`。保存截断信息与完整输出路径后停止 loader，并重建显示。
    pub fn set_complete(
        &mut self,
        exit_code: Option<i32>,
        cancelled: bool,
        truncation_result: Option<TruncationResult>,
        full_output_path: Option<String>,
    ) {
        self.exit_code = exit_code;
        self.status = if cancelled {
            BashStatus::Cancelled
        } else {
            match exit_code {
                Some(code) if code != 0 => BashStatus::Error,
                _ => BashStatus::Complete,
            }
        };
        self.truncation_result = truncation_result;
        self.full_output_path = full_output_path;
        self.loader.stop();
        self.update_display();
    }

    fn update_display(&mut self) {
        // 阶段 1：按上下文上限从尾部截断。折叠/展开、状态行都基于截断后的行集合，
        // 而不是 output_lines 原文；原文仍由 get_output 完整返回。
        // 与 bash 工具相同的上下文上限截断。
        let full_output = self.output_lines.join("\n");
        let context_truncation = truncate_tail(
            &full_output,
            &TruncationOptions {
                max_lines: Some(DEFAULT_MAX_LINES),
                max_bytes: Some(DEFAULT_MAX_BYTES),
            },
        );
        let available_lines: Vec<String> = if context_truncation.content.is_empty() {
            Vec::new()
        } else {
            context_truncation
                .content
                .split('\n')
                .map(str::to_string)
                .collect()
        };
        // 阶段 2：折叠时取最后 PREVIEW_LINES 条逻辑行；隐藏数按逻辑行算，
        // 实际换行折叠由 CollapsedPreview 在渲染时按宽度完成。
        let preview_logical_lines: Vec<String> = available_lines
            .iter()
            .skip(available_lines.len().saturating_sub(PREVIEW_LINES))
            .cloned()
            .collect();
        let hidden_line_count = available_lines.len() - preview_logical_lines.len();

        self.content_container.clear();

        let header = Text::with_padding(
            fg(self.color_key, &bold(&format!("$ {}", self.command))),
            self.output_pad,
            0,
        );
        self.content_container.add_child(Box::new(header));

        if !available_lines.is_empty() {
            if self.expanded {
                // 展开：全部可用行原样成段（仅在 update_display 时拼字符串，
                // 不在 render 里重复拼接）。
                let display_text = available_lines
                    .iter()
                    .map(|line| fg(ThemeColor::Muted, line))
                    .collect::<Vec<String>>()
                    .join("\n");
                self.content_container
                    .add_child(Box::new(Text::with_padding(
                        format!("\n{display_text}"),
                        self.output_pad,
                        0,
                    )));
            } else {
                let styled_output = preview_logical_lines
                    .iter()
                    .map(|line| fg(ThemeColor::Muted, line))
                    .collect::<Vec<String>>()
                    .join("\n");
                let styled_input = format!("\n{styled_output}");
                self.content_container
                    .add_child(Box::new(CollapsedPreview::new(
                        styled_input,
                        self.output_pad,
                    )));
            }
        }

        // 状态行只在执行结束后出现；运行中由 loader 承担“正在进行”的表达。
        if self.status != BashStatus::Running {
            let mut status_parts: Vec<String> = Vec::new();
            if hidden_line_count > 0 {
                // 展开态提示如何收起，折叠态提示还剩多少行未显示。
                if self.expanded {
                    status_parts.push(format!(
                        "{}{}{}",
                        fg(ThemeColor::Muted, "("),
                        key_hint("app.tools.expand", "to collapse"),
                        fg(ThemeColor::Muted, ")"),
                    ));
                } else {
                    status_parts.push(format!(
                        "{}{}{}",
                        fg(
                            ThemeColor::Muted,
                            &format!("... {hidden_line_count} more lines (")
                        ),
                        key_hint("app.tools.expand", "to expand"),
                        fg(ThemeColor::Muted, ")"),
                    ));
                }
            }

            if self.status == BashStatus::Cancelled {
                status_parts.push(fg(ThemeColor::Warning, "(cancelled)"));
            } else if self.status == BashStatus::Error {
                status_parts.push(fg(
                    ThemeColor::Error,
                    &format!("(exit {})", self.exit_code.unwrap_or_default()),
                ));
            }

            // 两种截断任一发生就要提示：bash 工具侧的截断（truncation_result）或
            // 本组件为上下文做的那次截断（context_truncation）。
            let was_truncated = self
                .truncation_result
                .as_ref()
                .map(|result| result.truncated)
                .unwrap_or(false)
                || context_truncation.truncated;
            // TS `if (wasTruncated && this.fullOutputPath)`：空字符串是 falsy。
            if was_truncated
                && let Some(full_output_path) = &self.full_output_path
                && !full_output_path.is_empty()
            {
                status_parts.push(fg(
                    ThemeColor::Warning,
                    &format!("Output truncated. Full output: {full_output_path}"),
                ));
            }

            if !status_parts.is_empty() {
                self.content_container
                    .add_child(Box::new(Text::with_padding(
                        format!("\n{}", status_parts.join("\n")),
                        self.output_pad,
                        0,
                    )));
            }
        }
    }

    /// `getOutput()`：已剥 ANSI、已归一换行的原始输出。
    pub fn get_output(&self) -> String {
        self.output_lines.join("\n")
    }

    /// `getCommand()`。
    pub fn get_command(&self) -> &str {
        &self.command
    }

    /// 是否仍在运行。
    pub fn is_running(&self) -> bool {
        self.status == BashStatus::Running
    }

    /// 安装 loader 的动画节拍器（TS 的 `setInterval`；见 [`Loader::set_timer`]）。
    pub fn set_timer(&mut self, timer: Box<dyn Timer>) {
        self.loader.set_timer(timer);
    }

    /// 底层 loader（测试/宿主查询用）。
    pub fn loader(&self) -> &Loader {
        &self.loader
    }

    /// 底层 loader（可变）。
    pub fn loader_mut(&mut self) -> &mut Loader {
        &mut self.loader
    }
}

impl Component for BashExecutionComponent {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn render(&mut self, width: usize) -> Vec<String> {
        // 顺序即屏幕上的自上而下顺序；loader 只在运行中出现，且它自己会前置一个空行。
        let mut lines = self.spacer.render(width);
        lines.extend(self.top_border.render(width));
        lines.extend(self.content_container.render(width));
        if self.status == BashStatus::Running {
            lines.extend(self.loader.render(width));
        }
        lines.extend(self.bottom_border.render(width));
        lines
    }

    fn handle_mouse(
        &mut self,
        _event: &kiana_tui::tui::TuiMouseEvent,
    ) -> Option<kiana_tui::tui::TuiMouseEventResult> {
        None
    }

    fn invalidate(&mut self) {
        // 内容容器与 loader 都缓存了渲染结果；清完后立即重建内容，
        // 保证下一次 render 看到的是当前状态（主题切换等场景依赖这里）。
        self.content_container.invalidate();
        Component::invalidate(&mut self.loader);
        self.update_display();
    }

    fn handles_input(&self) -> bool {
        false
    }
}

/// 供测试/宿主检查的可见宽度辅助（宽度不变式断言用）。
///
/// 返回终端显示列数：ANSI 转义不计入，TAB 先展开为 3 个空格，其余按字素宽度累加
/// （实现见 `kiana_tui::text::visible_width`）。
pub fn line_visible_width(line: &str) -> usize {
    visible_width(line)
}
