//! core/tools/renderers/find.ts。
//!
//! `find` 的 TUI 渲染：调用行 `find {pattern} in {path}`（可选 `(limit N)`），
//! 结果前 20 行预览，警告 `[Truncated: {n} results limit, {size} limit]`。
//!
//! # 参数与展示
//! - 调用行：`pattern` 用强调色；`path` 先做 `~` 压缩，空串显示成 `.`；
//! - `pattern` / `path` 不是字符串时显示 `[invalid arg]`（缺失/null 视作空串，不报错）；
//! - `limit` 只要存在就追加 `(limit N)`，`N` 用 JS `String(value)` 输出（`null` 会显示
//!   `(limit null)`，与 TS 模板字符串一致）。
//!
//! # 结果预览与警告
//! - 折叠时只显示前 20 行，其余行数追加 `... (N more lines, <键位> to expand)`；
//!   `options.expanded` 为真时显示全部；
//! - `details.resultLimitReached` 为真值（JS 语义，`0`/null/缺失都不算）时追加
//!   `"{n} results limit"`；
//! - `details.truncation.truncated` 为真时追加 `"{formatSize(maxBytes ?? 50KiB)} limit"`；
//! - 两个警告用 `", "` 连接进同一行 `[Truncated: ...]`，顺序固定为结果数、字节数。
//!
//! 警告读取一律通过 `super::details` 的宽松访问，缺失字段不会让渲染失败。

use std::sync::Arc;

use kiana_agent_core::AgentToolResult;
use kiana_ai::types::JsonValue;
use kiana_ext_api::tool_types::{
    RenderCallFn, RenderResultFn, ToolRenderContext, ToolRenderResultOptions, ToolRenderers,
};
use kiana_theme::theme::Theme;
use kiana_theme::theme_tokens::ThemeColor;
use kiana_tui::components::text_component::Text;
use kiana_tui::tui::Component;
use kiana_ui_kit::keybinding_hints::key_hint;

use super::details;
use crate::render_utils::{get_text_output, invalid_arg_text, shorten_path_str, str};

/// `formatFindCall(args, theme)`：调用行。
///
/// 重点在空值与类型错误的区分：`str()` 对缺失/null 返回 `Some("")`，对非字符串返回
/// `None`；因此 `path: ""` 会显示 `.`，而 `path: 42` 显示 `[invalid arg]`。
fn format_find_call(args: Option<&JsonValue>, theme: &Theme) -> String {
    let object = args.and_then(JsonValue::as_object);
    let pattern = str(object.and_then(|object| object.get("pattern")));
    let raw_path = str(object.and_then(|object| object.get("path")));
    let path = raw_path.map(|path| shorten_path_str(if path.is_empty() { "." } else { &path }));
    let limit = object.and_then(|object| object.get("limit"));
    let invalid_arg = invalid_arg_text(theme);
    let mut text = theme.fg(ThemeColor::ToolTitle, &theme.bold("find"))
        + " "
        + &match pattern {
            None => invalid_arg.clone(),
            Some(pattern) => theme.fg(ThemeColor::Accent, &pattern),
        }
        + &theme.fg(
            ThemeColor::ToolOutput,
            &format!(" in {}", path.unwrap_or(invalid_arg)),
        );
    if let Some(limit) = limit {
        text.push_str(&theme.fg(
            ThemeColor::ToolOutput,
            &format!(" (limit {})", kiana_json::js_string::js_string(Some(limit))),
        ));
    }
    text
}

/// `formatFindResult(result, options, theme, showImages)`：结果区文本。
///
/// 先 `get_text_output` 拼接文本块（按需把图片转成占位行）再做 JS `trim()`；空输出
/// （包括只有空白）不画任何行，但仍可能因 details 渲染警告行。`find` 不按语言高亮，
/// 所有预览行统一用 `ToolOutput` 颜色。
fn format_find_result(
    result: &AgentToolResult<JsonValue>,
    options: &ToolRenderResultOptions,
    theme: &Theme,
    show_images: bool,
) -> String {
    let output =
        kiana_json::js_string::js_trim(&get_text_output(Some(result), show_images)).to_string();
    let mut text = String::new();
    if !output.is_empty() {
        let lines: Vec<&str> = output.split('\n').collect();
        let max_lines = if options.expanded { lines.len() } else { 20 };
        let display_lines = &lines[..lines.len().min(max_lines)];
        let remaining = lines.len().saturating_sub(max_lines);
        text.push_str(&format!(
            "\n{}",
            display_lines
                .iter()
                .map(|line| theme.fg(ThemeColor::ToolOutput, line))
                .collect::<Vec<_>>()
                .join("\n")
        ));
        if remaining > 0 {
            text.push_str(&theme.fg(
                ThemeColor::Muted,
                &format!("\n... ({remaining} more lines,"),
            ));
            text.push(' ');
            text.push_str(&key_hint("app.tools.expand", "to expand"));
            text.push_str(&theme.fg(ThemeColor::Muted, ")"));
        }
    }

    let details = Some(&result.details);
    let result_limit = details::property(details, "resultLimitReached")
        .filter(|value| details::truthy(Some(*value)));
    let truncated = details::truncation_truncated(details);
    if result_limit.is_some() || truncated {
        let mut warnings: Vec<String> = Vec::new();
        if let Some(result_limit) = result_limit {
            warnings.push(format!(
                "{} results limit",
                details::text(Some(result_limit))
            ));
        }
        if truncated {
            warnings.push(format!(
                "{} limit",
                details::truncation_max_bytes_size(details)
            ));
        }
        text.push_str(&format!(
            "\n{}",
            theme.fg(
                ThemeColor::Warning,
                &format!("[Truncated: {}]", warnings.join(", ")),
            )
        ));
    }
    text
}

/// `findRenderers`：返回 `find` 的调用/结果渲染器。
///
/// 两个槽都不读写 `RenderState`，每次调用返回新的 [`Text`]；`render_shell` 为 `None`，
/// 由消费方画默认外壳。
pub fn find_renderers() -> ToolRenderers {
    let render_call: RenderCallFn = Arc::new(|args, theme, _context: &ToolRenderContext| {
        let text = format_find_call(Some(args), theme);
        Box::new(Text::with_padding(text, 0, 0)) as Box<dyn Component>
    });

    let render_result: RenderResultFn =
        Arc::new(|result, options, theme, context: &ToolRenderContext| {
            let text = format_find_result(result, options, theme, context.show_images);
            Box::new(Text::with_padding(text, 0, 0)) as Box<dyn Component>
        });

    ToolRenderers {
        render_shell: None,
        render_call: Some(render_call),
        render_result: Some(render_result),
    }
}
