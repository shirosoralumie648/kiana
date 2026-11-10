//! core/tools/renderers/grep.ts。
//!
//! `grep` 的 TUI 渲染：调用行 `grep /{pattern}/ in {path}`（可选 `({glob})` 与
//! ` limit {n}`），结果前 15 行预览，警告
//! `[Truncated: {n} matches limit, {size} limit, some lines truncated]`。
//!
//! # 参数与展示
//! - `pattern` 用强调色包成 `/.../`；`path` 先做 `~` 压缩，空串显示成 `.`；
//! - `pattern` / `path` 不是字符串时显示 `[invalid arg]`；缺失/null 视作空串；
//! - `glob` 只在非空字符串时追加 `({glob})`；`limit` 只要存在就追加 ` limit N`，
//!   `N` 用 JS `String(value)` 输出。
//!
//! # 结果预览与警告
//! - 折叠时只显示前 15 行，其余行数追加展开提示；`options.expanded` 为真时显示全部；
//! - 警告按固定顺序拼接：`details.matchLimitReached`（真值）→
//!   `details.truncation.truncated`（`"{formatSize(maxBytes ?? 50KiB)} limit"`）→
//!   `details.linesTruncated`（固定文案 `some lines truncated`）。注意 `linesTruncated`
//!   是 details 顶层字段，不在 `truncation` 下；
//! - 三个判断都走 JS 真值语义，`0`/null/缺失均不产生警告。
//!
//! 警告读取一律通过 `super::details` 的宽松访问，旧记录缺字段时不会渲染失败。

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

/// `formatGrepCall(args, theme)`：调用行。
///
/// `str()` 对缺失/null 返回 `Some("")`，对非字符串返回 `None`；因此缺失的 `pattern`
/// 渲染成 `grep // in .`，而 `pattern: 42` 渲染成 `[invalid arg]`（`grep` 的测试锁定）。
fn format_grep_call(args: Option<&JsonValue>, theme: &Theme) -> String {
    let object = args.and_then(JsonValue::as_object);
    let pattern = str(object.and_then(|object| object.get("pattern")));
    let raw_path = str(object.and_then(|object| object.get("path")));
    let path = raw_path.map(|path| shorten_path_str(if path.is_empty() { "." } else { &path }));
    let glob = str(object.and_then(|object| object.get("glob")));
    let limit = object.and_then(|object| object.get("limit"));
    let invalid_arg = invalid_arg_text(theme);
    let mut text = theme.fg(ThemeColor::ToolTitle, &theme.bold("grep"))
        + " "
        + &match pattern {
            None => invalid_arg.clone(),
            Some(pattern) => theme.fg(ThemeColor::Accent, &format!("/{pattern}/")),
        }
        + &theme.fg(
            ThemeColor::ToolOutput,
            &format!(" in {}", path.unwrap_or(invalid_arg)),
        );
    if let Some(glob) = glob
        && !glob.is_empty()
    {
        text.push_str(&theme.fg(ThemeColor::ToolOutput, &format!(" ({glob})")));
    }
    if let Some(limit) = limit {
        text.push_str(&theme.fg(
            ThemeColor::ToolOutput,
            &format!(" limit {}", kiana_json::js_string::js_string(Some(limit))),
        ));
    }
    text
}

/// `formatGrepResult(result, options, theme, showImages)`：结果区文本。
///
/// 先 `get_text_output` 拼接文本块并做 JS `trim()`；空输出（包括只有空白）不画预览行，
/// 但 details 仍可能追加警告行。所有预览行用 `ToolOutput` 颜色。
fn format_grep_result(
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
        let max_lines = if options.expanded { lines.len() } else { 15 };
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
    let match_limit = details::property(details, "matchLimitReached")
        .filter(|value| details::truthy(Some(*value)));
    let truncated = details::truncation_truncated(details);
    let lines_truncated = details::truthy(details::property(details, "linesTruncated"));
    if match_limit.is_some() || truncated || lines_truncated {
        let mut warnings: Vec<String> = Vec::new();
        if let Some(match_limit) = match_limit {
            warnings.push(format!(
                "{} matches limit",
                details::text(Some(match_limit))
            ));
        }
        if truncated {
            warnings.push(format!(
                "{} limit",
                details::truncation_max_bytes_size(details)
            ));
        }
        if lines_truncated {
            warnings.push("some lines truncated".to_string());
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

/// `grepRenderers`：返回 `grep` 的调用/结果渲染器。
///
/// 两个槽都不读写 `RenderState`，每次调用返回新的 [`Text`]；`render_shell` 为 `None`，
/// 由消费方画默认外壳。
pub fn grep_renderers() -> ToolRenderers {
    let render_call: RenderCallFn = Arc::new(|args, theme, _context: &ToolRenderContext| {
        let text = format_grep_call(Some(args), theme);
        Box::new(Text::with_padding(text, 0, 0)) as Box<dyn Component>
    });

    let render_result: RenderResultFn =
        Arc::new(|result, options, theme, context: &ToolRenderContext| {
            let text = format_grep_result(result, options, theme, context.show_images);
            Box::new(Text::with_padding(text, 0, 0)) as Box<dyn Component>
        });

    ToolRenderers {
        render_shell: None,
        render_call: Some(render_call),
        render_result: Some(render_result),
    }
}
