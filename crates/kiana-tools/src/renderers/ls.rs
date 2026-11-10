//! core/tools/renderers/ls.ts。
//!
//! `ls` 工具的 TUI 渲染：调用行的路径（缺省 `.`）与 `(limit N)`，结果行前 20 行预览
//! 与逐字警告 `[Truncated: {n} entries limit, {size} limit]`。
//!
//! # 参数与展示
//! - 调用行：路径走 `render_tool_path`（`~` 压缩 + 终端支持时加 OSC 8 文件链接），
//!   缺失或空串回落到 `.`；非字符串路径显示 `[invalid arg]`；
//! - `limit` 只要存在就追加 `(limit N)`，`N` 用 JS `String(value)` 输出，因此
//!   `2.5`、`"abc"`、`null`、`true` 都会原样出现在括号里（`ls` 的测试逐项锁定）。
//!
//! # 结果预览与警告
//! - 输出先做 JS `trim()`（去掉 U+FEFF、保留 U+0085），只有空白时整段不渲染；
//! - 折叠时显示前 20 行，其余行数追加展开提示；`options.expanded` 为真时显示全部；
//! - `details.entryLimitReached` 为真值时追加 `"{n} entries limit"`（`0` 是 falsy，
//!   不产生警告）；
//! - `details.truncation.truncated` 为真时追加 `"{formatSize(maxBytes ?? 50KiB)} limit"`；
//! - 两个警告用 `", "` 连接进同一行 `[Truncated: ...]`，顺序固定为条目数、字节数。
//!
//! 警告读取通过 `super::details` 的宽松访问，旧记录缺字段时不会渲染失败。

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

use super::details;
use crate::render_utils::{RenderToolPathOptions, get_text_output, render_tool_path, str};

/// `formatLsCall(args, theme, cwd)`：调用行。
///
/// `RenderToolPathOptions::empty_fallback = "."` 只作用于「`path` 是空串/缺失」的情况；
/// `path` 是数字等非字符串类型时 `str()` 返回 `None`，直接渲染 `[invalid arg]`，不会回落到
/// `.`。`cwd` 只用于把相对路径转成超链接 URL（终端不支持超链接时不使用）。
fn format_ls_call(args: Option<&JsonValue>, theme: &Theme, cwd: &str) -> String {
    let limit = args
        .and_then(JsonValue::as_object)
        .and_then(|object| object.get("limit"));
    let options = RenderToolPathOptions {
        empty_fallback: Some(".".to_string()),
    };
    let path_display = render_tool_path(
        str(args
            .and_then(JsonValue::as_object)
            .and_then(|object| object.get("path")))
        .as_deref(),
        theme,
        cwd,
        Some(&options),
    );
    let mut text = format!(
        "{} {path_display}",
        theme.fg(ThemeColor::ToolTitle, &theme.bold("ls"))
    );
    if let Some(limit) = limit {
        text.push_str(&theme.fg(
            ThemeColor::ToolOutput,
            &format!(" (limit {})", js_number_text(limit)),
        ));
    }
    text
}

/// JS 模板字符串里的 `${value}`（`String(value)`）：数字按 ECMAScript 规则，
/// 非字符串值（`null`/布尔/数组/对象）也按同一规则转换。
fn js_number_text(value: &JsonValue) -> String {
    kiana_json::js_string::js_string(Some(value))
}

/// `formatLsResult(result, options, theme, showImages)`：结果区文本。
///
/// 输出空（含只有空白）时连预览行与其前导换行都不画；但 warning 行独立于预览，只要
/// details 命中就会追加。所有预览行统一 `ToolOutput` 颜色，不做语法高亮。
fn format_ls_result(
    result: &AgentToolResult<JsonValue>,
    options: &ToolRenderResultOptions,
    theme: &Theme,
    show_images: bool,
) -> String {
    // TS 是 `getTextOutput(...).trim()`：用 JS 的 WhiteSpace 集合（去掉 U+FEFF、
    // 保留 U+0085），不是 Rust 的 `str::trim`。
    let output = get_text_output(Some(result), show_images);
    let output = kiana_json::js_string::js_trim(&output);
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
            text.push_str(&kiana_ui_kit::keybinding_hints::key_hint(
                "app.tools.expand",
                "to expand",
            ));
            text.push_str(&theme.fg(ThemeColor::Muted, ")"));
        }
    }

    let details = Some(&result.details);
    let entry_limit = details::property(details, "entryLimitReached")
        .filter(|value| details::truthy(Some(*value)));
    let truncated = details::truncation_truncated(details);
    if entry_limit.is_some() || truncated {
        let mut warnings: Vec<String> = Vec::new();
        if let Some(entry_limit) = entry_limit {
            warnings.push(format!(
                "{} entries limit",
                details::text(Some(entry_limit))
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

/// `lsRenderers`：返回 `ls` 的调用/结果渲染器。
///
/// 两个槽都不读写 `RenderState`；`render_call` 会读取 `context.cwd` 生成相对路径的
/// 超链接，其余与其它简单工具渲染器一致。
pub fn ls_renderers() -> ToolRenderers {
    let render_call: RenderCallFn = Arc::new(|args, theme, context: &ToolRenderContext| {
        let text = format_ls_call(Some(args), theme, &context.cwd.to_string_lossy());
        Box::new(Text::with_padding(text, 0, 0)) as Box<dyn Component>
    });

    let render_result: RenderResultFn =
        Arc::new(|result, options, theme, context: &ToolRenderContext| {
            let text = format_ls_result(result, options, theme, context.show_images);
            Box::new(Text::with_padding(text, 0, 0)) as Box<dyn Component>
        });

    ToolRenderers {
        render_shell: None,
        render_call: Some(render_call),
        render_result: Some(render_result),
    }
}
