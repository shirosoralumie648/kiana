//! core/tools/renderers/bash.ts。
//!
//! shell 工具（bash / powershell）的 TUI 渲染：调用行 `$ {command}` / `PS> {command}`
//! 与 `(timeout Ns)`，结果行的折叠预览（5 个可视行、`keep=end`）或展开视图，
//! 以及 `[Full output: ... . Truncated: ...]` 警告行。
//!
//! `BASH_UPDATE_THROTTLE_MS` 从这里导出给 `bash.rs`（TS 的依赖方向是 bash.ts →
//! renderers/bash.ts）。
//!
//! 组件复用：TS 用 `context.lastComponent` 原地 `setText`；Rust 的 `lastComponent` 是借用，
//! 因此每次返回新 [`Text`] / [`Container`]，渲染输出完全相同。
//!
//! # 渲染流程
//! `render_call` 只画头部：先由 `start_tool_timer` 记录开始时间（`execution_started` 为假
//! 时直接跳过），再拼 `$ command` 与可选的 `(timeout Ns)`。
//!
//! `render_result` 每帧重建整个 [`Container`]，顺序固定：
//! 1. 用 `get_text_output` 取纯文本并做 JS `trim()`（会去掉首尾空白）；
//! 2. 最终结果（`is_partial == false`）、`truncation.truncated` 为真且
//!    `fullOutputPath` 为真值时，删掉文本尾部与下方警告行重复的
//!    `[Showing ... Full output: path]` 脚注——只匹配末尾 `]` 前最后一个 `"\n\n["`
//!    且其后含 `fullOutputPath` 文本；
//! 3. 展开时原样输出；折叠时用 [`VisualLinePreview`] 保留最后 [`BASH_PREVIEW_LINES`] 个
//!    可视行（`Keep::End`）并附展开提示；
//! 4. 追加 `[Full output: ... . Truncated: ...]` 警告行，最后追加计时文本（若有）。
//!
//! # 注意事项
//! - details 按 JS 属性访问语义宽松读取（见 `super::details`），旧记录/扩展结果缺字段时
//!   渲染照常进行，缺省上限回落到 `truncate` 的默认值。
//! - [`BASH_UPDATE_THROTTLE_MS`] 只是给 `bash.rs` 的流式更新循环使用的常量（TS 的依赖
//!   方向是 bash.ts → 本文件）；本渲染器不做节流，每帧全量重建。
//! - 计时文本的每秒刷新由 `crate::render_utils::tool_timer_text` 的后台线程驱动，本文件
//!   只消费它的返回值，不自己启动计时器。

use std::sync::Arc;

use kiana_agent_core::AgentToolResult;
use kiana_ai::types::JsonValue;
use kiana_ext_api::tool_types::{
    RenderCallFn, RenderResultFn, ToolRenderContext, ToolRenderResultOptions, ToolRenderers,
};
use kiana_theme::theme::Theme;
use kiana_theme::theme_tokens::ThemeColor;
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::Text;
use kiana_tui::tui::{Component, Container};
use kiana_ui_kit::keybinding_hints::key_hint;
use kiana_ui_kit::visual_truncate::{Keep, VisualLinePreview, VisualLinePreviewOptions};

use super::details;
use crate::render_utils::{
    get_text_output, invalid_arg_text, start_tool_timer, str, tool_timer_text,
};

/// `BASH_PREVIEW_LINES = 5`。
pub const BASH_PREVIEW_LINES: usize = 5;

/// 流式更新的最小间隔（毫秒）；`bash.ts` 用它做节流。
pub const BASH_UPDATE_THROTTLE_MS: u64 = 100;

/// JS 的 `String.prototype.trimEnd` 空白集合（`WhiteSpace + LineTerminator`）。
///
/// 与 Rust 的 [`str::trim_end`] 不同：JS 包含 U+FEFF，不包含 U+0085。
fn is_js_trim_end_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'..='\u{000d}'
            | ' '
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// JS `trimEnd()`。
fn js_trim_end(text: &str) -> &str {
    text.trim_end_matches(is_js_trim_end_whitespace)
}

/// JS 模板字符串的 `${value}`。
fn js_string(value: Option<&JsonValue>) -> String {
    kiana_json::js_string::js_string(value)
}

/// `formatShellCall(args, prompt)`：调用行头部。
///
/// `command` 缺失/null 会先经 `str()` 变成空串，再显示成 `...`（即 `$ ...`）；非字符串
/// （类型错误）显示 `[invalid arg]`。`timeout` 按 JS 真值判断：`0`、`false`、空串、null
/// 都不加 `(timeout Ns)` 后缀；真值时按 JS `String(value)` 拼接。
fn format_shell_call(args: Option<&JsonValue>, prompt: &str, theme: &Theme) -> String {
    let object = args.and_then(JsonValue::as_object);
    let command = str(object.and_then(|object| object.get("command")));
    let timeout = object.and_then(|object| object.get("timeout"));
    let timeout_suffix = if kiana_json::js_string::is_truthy(timeout) {
        theme.fg(
            ThemeColor::Muted,
            &format!(" (timeout {}s)", js_string(timeout)),
        )
    } else {
        String::new()
    };
    let command_display = match command {
        None => invalid_arg_text(theme),
        Some(command) if command.is_empty() => theme.fg(ThemeColor::ToolOutput, "..."),
        Some(command) => command,
    };
    theme.fg(
        ThemeColor::ToolTitle,
        &theme.bold(&format!("{prompt} {command_display}")),
    ) + &timeout_suffix
}

/// `rebuildBashResultRenderComponent(component, result, options, showImages, timerText)`。
///
/// 清空并重建结果区。`show_images` 仅在终端支持图片时起作用：为假时把图片块替换成
/// 占位行；终端不支持图片时无论如何都输出占位行（`get_text_output` 的条件是
/// `capabilities.images.is_none() || !show_images`）。
/// `timer_text` 为 `None` 时不画耗时行（调用方已按 `is_partial`/`is_error` 算好）。
///
/// 细节：文本切分按 `'\n'` 逐行上色，但折叠预览的换行由 `VisualLinePreview` 按终端宽度
/// 重新折行，因此“5 行”是可视行而不是源文本行。`Truncated:` 文案的两个分支由
/// `truncation.truncatedBy == "lines"` 区分，分别拼出「显示行数/总行数」与
/// 「显示行数/字节上限」两种文案。
fn rebuild_bash_result_render_component(
    component: &mut Container,
    result: &AgentToolResult<JsonValue>,
    options: &ToolRenderResultOptions,
    show_images: bool,
    timer_text: Option<&str>,
    theme: &Theme,
) {
    component.clear();

    let mut output =
        kiana_json::js_string::js_trim(&get_text_output(Some(result), show_images)).to_string();
    // TS 从 `result.details?.[...]` 逐字段读取（见 [`details`]）：details 来自旧版本 kiana /
    // 扩展、缺少 `lastLinePartial` 等字段时仍然可用。
    let details = Some(&result.details);
    let truncated = details::truncation_truncated(details);
    // TS: `fullOutputPath` 按真值使用（空串/0/null 都视为缺失）。
    let full_output_path =
        details::property(details, "fullOutputPath").filter(|value| details::truthy(Some(*value)));

    // 最终结果去掉与下方 warning 行重复的 `[... Full output: path]` 脚注。
    if !options.is_partial
        && truncated
        && let Some(full_output_path) = full_output_path
        && output.ends_with(']')
        && let Some(footer_start) = output.rfind("\n\n[")
        && output[footer_start..].contains(&details::text(Some(full_output_path)))
    {
        output = js_trim_end(&output[..footer_start]).to_string();
    }

    if !output.is_empty() {
        let styled_output = output
            .split('\n')
            .map(|line| theme.fg(ThemeColor::ToolOutput, line))
            .collect::<Vec<_>>()
            .join("\n");
        if options.expanded {
            component.add_child(Box::new(Text::with_padding(
                format!("\n{styled_output}"),
                0,
                0,
            )));
        } else {
            component.add_child(Box::new(Spacer::with_lines(1.0)));
            component.add_child(Box::new(VisualLinePreview::new(VisualLinePreviewOptions {
                text: styled_output,
                max_visual_lines: BASH_PREVIEW_LINES,
                keep: Keep::End,
                // TS 的 `theme` 是全局 Proxy：hint 在渲染时读取当前主题，
                // 不捕获构造时的主题实例。
                format_hint: Box::new(|hidden| {
                    let muted = |text: &str| {
                        kiana_theme::theme::THEME
                            .fg(ThemeColor::Muted, text)
                            .unwrap_or_else(|_| text.to_string())
                    };
                    muted(&format!("... ({hidden} earlier lines,"))
                        + " "
                        + &key_hint("app.tools.expand", "to expand")
                        + &muted(")")
                }),
            })));
        }
    }

    if truncated || full_output_path.is_some() {
        let mut warnings: Vec<String> = Vec::new();
        if let Some(full_output_path) = full_output_path {
            warnings.push(format!(
                "Full output: {}",
                details::text(Some(full_output_path))
            ));
        }
        if truncated {
            if details::truncated_by_lines(details) {
                warnings.push(format!(
                    "Truncated: showing {} of {} lines",
                    details::truncation_text(details, "outputLines"),
                    details::truncation_text(details, "totalLines")
                ));
            } else {
                warnings.push(format!(
                    "Truncated: {} lines shown ({} limit)",
                    details::truncation_text(details, "outputLines"),
                    details::truncation_max_bytes_size(details)
                ));
            }
        }
        component.add_child(Box::new(Text::with_padding(
            format!(
                "\n{}",
                theme.fg(ThemeColor::Warning, &format!("[{}]", warnings.join(". ")))
            ),
            0,
            0,
        )));
    }

    if let Some(timer_text) = timer_text {
        component.add_child(Box::new(Text::with_padding(
            format!("\n{}", theme.fg(ThemeColor::Muted, timer_text)),
            0,
            0,
        )));
    }
}

/// `createShellRenderers(prompt)`：bash 与 powershell 只差提示符。
///
/// 返回的集合不带 `render_shell`（交给消费方的默认外壳）；`prompt` 被复制进
/// `render_call` 闭包，内置映射里分别是 `"$"`（bash）和 `"PS>"`（powershell）。
/// `bash.rs` 的工具定义直接用本函数生成渲染器，因此提示符最终来自
/// `ShellToolConfig.prompt`。
///
/// `render_call` 会写共享 `RenderState`（`startedAt` 等），`render_result` 会读它；
/// 两者必须在同一个 tool call 的 context 上调用，才会共享计时状态。
pub fn create_shell_renderers(prompt: &str) -> ToolRenderers {
    let prompt = prompt.to_string();
    let call_prompt = prompt.clone();
    let render_call: RenderCallFn = Arc::new(move |args, theme, context: &ToolRenderContext| {
        start_tool_timer(context);
        let text = format_shell_call(Some(args), &call_prompt, theme);
        Box::new(Text::with_padding(text, 0, 0)) as Box<dyn Component>
    });

    let render_result: RenderResultFn =
        Arc::new(|result, options, theme, context: &ToolRenderContext| {
            let timer_text = tool_timer_text(context, options.is_partial);
            let mut component = Container::new();
            rebuild_bash_result_render_component(
                &mut component,
                result,
                options,
                context.show_images,
                timer_text.as_deref(),
                theme,
            );
            component.invalidate();
            Box::new(component) as Box<dyn Component>
        });

    ToolRenderers {
        render_shell: None,
        render_call: Some(render_call),
        render_result: Some(render_result),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_trim_end_uses_js_whitespace_set() {
        assert_eq!(js_trim_end("a\u{feff}"), "a");
        assert_eq!(js_trim_end("a\u{0085}"), "a\u{0085}");
        assert_eq!(js_trim_end("a \n\t"), "a");
    }
}
