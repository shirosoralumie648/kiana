//! core/tools/renderers/write.ts。
//!
//! `write` 工具的 TUI 渲染：调用行的高亮缓存（全量 / 增量）与 10 行内容预览，
//! 结果行只在错误时显示文本。
//!
//! # 高亮缓存
//!
//! TS 把缓存挂在 `WriteCallRenderComponent extends Text` 上，靠 `lastComponent`
//! 原地复用。Rust 的 [`ToolRenderContext::last_component`] 是借用，渲染器无法返回同一个
//! 实例，因此定义自己的 [`WriteCallRenderComponent`]（包装 [`Text`]），通过
//! [`kiana_tui::Component::as_any`] 从上一帧的组件里取出缓存并克隆；没有上一帧时全量重建。
//! 缓存输出必须与全量重建完全一致，纯属性能优化（流式参数每帧只高亮新增行 + 刷新前 50 行）。
//!
//! # 在系统中的位置
//!
//! 由 `crate::write::create_write_tool_definition` 通过 [`write_renderers`] 装进工具
//! 定义，`render_shell` 为 `None`（默认外壳）。宿主（如 `kiana-interactive` 的
//! `ToolExecutionComponent`）在参数流式增长期间每次重新渲染都会把上一帧组件（可能为
//! `None`）交回并调用 `render_call`；`context.args_complete` 为真表示这一轮参数已经完整。
//!
//! # 一次调用行的渲染
//!
//! 1. `file_path ?? path` 取路径，`content` 用 `str` 取字符串：缺失/`null` → 空串，
//!    类型不是字符串 → `None`。
//! 2. 只有 `content` 是字符串时才维护高亮缓存：参数完整 → 全量重建；流式中 →
//!    尝试在上一帧缓存上增量追加；类型错误 → 丢弃缓存。
//! 3. 用缓存（没有缓存则现场计算）渲染内容：折叠最多 10 行，展开显示全部；非字符串
//!    `content` 渲染 `[invalid content arg - expected string]`。
//!
//! # 注意事项
//!
//! - 增量路径只在「语言相同、`raw_path` 相同、新内容以旧内容为字节前缀」时继续；
//!   内容变短或被改写会整段重建；前缀一致但 UTF-16 长度相同说明内容没变，直接复用；
//!   取不到语言时缓存为 `None`。
//! - `content` 类型错误与空串不同：前者显示错误文案，后者只有标题行。
//! - 结果渲染器只在错误时输出。它克隆结果后用 `context.is_error` 覆盖
//!   `result.is_error`（对应 TS 的 `{ ...result, isError: context.isError }`），
//!   因此 `AgentToolResult` 里缺省的 `is_error` 不会吞掉错误行。

use std::any::Any;
use std::sync::Arc;

use kiana_agent_core::AgentToolResult;
use kiana_ai::types::JsonValue;
use kiana_ext_api::tool_types::{
    RenderCallFn, RenderResultFn, ToolRenderContext, ToolRenderResultOptions, ToolRenderers,
};
use kiana_theme::theme::{Theme, get_language_from_path, highlight_code};
use kiana_theme::theme_tokens::ThemeColor;
use kiana_tui::components::text_component::Text;
use kiana_tui::tui::{Component, Container};

use crate::render_utils::{normalize_display_text, render_tool_path, replace_tabs, str};

/// `WRITE_PARTIAL_FULL_HIGHLIGHT_LINES = 50`。
///
/// 增量更新后，前 50 行会拼回一整段重新高亮（而不是逐行），让跨行语法结构在这些行内
/// 保持一致；超出的行仍然只按单行高亮。
const WRITE_PARTIAL_FULL_HIGHLIGHT_LINES: usize = 50;

/// `WriteHighlightCache`。
///
/// 流式参数通常只在尾部追加；缓存让常见增长路径只重算新增的行，内容变短或被改写时
/// 整段重建。维护时有两个不变量：
/// `normalized_lines.len() == highlighted_lines.len()`（更新时成对追加，重建时都来自
/// 同一份行切分），`raw_content` 保存产生这些行的**原始**内容（未删 `\r`、未替换
/// tab），下一帧用它做前缀比较和按字节切片。
///
/// 本模块只在能确定语言时创建缓存（此时 `raw_path` 非空）；`Default` 或外部直接构造
/// 可能得到其它状态，下一次增量更新会因取不到语言而返回 `None`，或按「语言/路径
/// 不匹配」整段重建。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WriteHighlightCache {
    /// 缓存对应的原始路径（`None` 表示没有语言）。
    pub raw_path: Option<String>,
    /// 高亮语言。
    pub lang: String,
    /// 原始内容（用于判断增量前缀）。
    pub raw_content: String,
    /// `normalizeDisplayText` + `replaceTabs` 之后的行。
    pub normalized_lines: Vec<String>,
    /// 高亮后的行。
    pub highlighted_lines: Vec<String>,
}

/// `WriteCallRenderComponent`：携带高亮缓存的文本组件。
///
/// 每次 `render_call` 被调用时由 `write_renderers` 新建并返回；下一次渲染时宿主通过
/// [`ToolRenderContext::last_component`] 交回，渲染器再用 [`Component::as_any`]
/// 下转型取回 `cache`。`cache` 是公开字段，测试也据此检查增量更新的结果。
pub struct WriteCallRenderComponent {
    text: Text,
    /// 高亮缓存。
    pub cache: Option<WriteHighlightCache>,
}

impl WriteCallRenderComponent {
    /// 用给定文本与缓存构造（paddingX/paddingY 与 TS 的 `new Text("", 0, 0)` 一致）。
    ///
    /// 不检查 `text` 与 `cache` 是否描述同一份内容；保证二者一致是调用方的责任。
    pub fn new(text: impl Into<String>, cache: Option<WriteHighlightCache>) -> Self {
        Self {
            text: Text::with_padding(text, 0, 0),
            cache,
        }
    }
}

impl Component for WriteCallRenderComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.text.render(width)
    }

    // 纯展示组件：返回 false 让宿主跳过输入分发（`Component::handles_input` 默认 true）。
    fn handles_input(&self) -> bool {
        false
    }

    // 转发给内部 Text，清空它的渲染缓存。
    fn invalidate(&mut self) {
        self.text.invalidate();
    }

    // 必须返回 `Some(self)`：默认实现是 `None`，`write_renderers` 的 `downcast_ref`
    // 会因此永远取不到上一帧的 `cache`。
    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    // 与 `as_any` 对称；当前渲染链路只用到只读下转型。
    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

/// `highlightSingleLine(line, lang)`。
///
/// `highlight_code` 对任意输入都按 `\n` 切成行数组返回，单行输入仍会有第一个元素；
/// `unwrap_or_default` 只防御意外的空数组。
fn highlight_single_line(line: &str, lang: &str) -> String {
    highlight_code(line, Some(lang))
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// `refreshWriteHighlightPrefix(cache)`：前 50 行整体重排高亮。
///
/// 把前 `min(50, 行数)` 行拼成一段文本交给 `highlight_code`，逐行写回缓存；高亮结果
/// 缺某个下标时退回该行的单行高亮。调用前提：`highlighted_lines` 至少和
/// `normalized_lines` 一样长（本模块的构造/更新路径保证两者等长）。
fn refresh_write_highlight_prefix(cache: &mut WriteHighlightCache) {
    let prefix_count = WRITE_PARTIAL_FULL_HIGHLIGHT_LINES.min(cache.normalized_lines.len());
    if prefix_count == 0 {
        return;
    }
    let prefix_source = cache.normalized_lines[..prefix_count].join("\n");
    let prefix_highlighted = highlight_code(&prefix_source, Some(&cache.lang));
    for index in 0..prefix_count {
        cache.highlighted_lines[index] = prefix_highlighted
            .get(index)
            .cloned()
            .unwrap_or_else(|| highlight_single_line(&cache.normalized_lines[index], &cache.lang));
    }
}

/// `rebuildWriteHighlightCacheFull(rawPath, fileContent)`：没有语言时返回 `None`。
///
/// 全量路径：`normalize_display_text`（删除 `\r`）→ `replace_tabs`（每个 tab 换三个
/// 空格）→ 整段 `highlight_code`。`raw_path` 为 `None`/空串或扩展名没有对应语言时
/// 返回 `None`。`raw_content` 保存**未规范化**的原文，供下一帧做前缀比较。
fn rebuild_write_highlight_cache_full(
    raw_path: Option<&str>,
    file_content: &str,
) -> Option<WriteHighlightCache> {
    let lang = raw_path
        .filter(|path| !path.is_empty())
        .and_then(get_language_from_path)?;
    let display_content = normalize_display_text(file_content);
    let normalized = replace_tabs(&display_content);
    Some(WriteHighlightCache {
        raw_path: raw_path.map(str::to_string),
        lang: lang.to_string(),
        raw_content: file_content.to_string(),
        normalized_lines: normalized.split('\n').map(str::to_string).collect(),
        highlighted_lines: highlight_code(&normalized, Some(lang)),
    })
}

/// `updateWriteHighlightCacheIncremental(cache, rawPath, fileContent)`。
///
/// 快速路径的条件：新路径能取到语言且与 `cache.lang` 一致、`raw_path` 与
/// `cache.raw_path` 一致、`file_content` 以 `cache.raw_content` 为**字节前缀**
/// （内容只在尾部增长）。取不到语言时函数返回 `None`；其它条件不满足时回到全量重建
/// （全量重建取不到语言时同样是 `None`）。前缀一致且 UTF-16 长度相同时内容未变，
/// 直接返回原缓存。
///
/// 否则按阶段更新：
/// 1. 按字节切出增量——`starts_with` 保证切点落在字符边界；
/// 2. 增量的第一段接到缓存最后一行尾部，并重新单行高亮该行；
/// 3. 其余段各成新行，逐行高亮（同时保持两个行数组等长）；
/// 4. 重排前 50 行，让跨行语法结构恢复上下文。
///
/// 返回的缓存描述的就是 `file_content`，可被下一帧继续增量更新。函数开头的
/// UTF-16 长度比较对应 TS 的 `fileContent.length === cache.rawContent.length`；
/// 长度不同但字节前缀成立时，按字节切片与 TS 的 `slice(旧 length)` 等价。
fn update_write_highlight_cache_incremental(
    cache: Option<WriteHighlightCache>,
    raw_path: Option<&str>,
    file_content: &str,
) -> Option<WriteHighlightCache> {
    let lang = raw_path
        .filter(|path| !path.is_empty())
        .and_then(get_language_from_path)?;
    let Some(mut cache) = cache else {
        return rebuild_write_highlight_cache_full(raw_path, file_content);
    };
    if cache.lang != lang || cache.raw_path.as_deref() != raw_path {
        return rebuild_write_highlight_cache_full(raw_path, file_content);
    }
    if !file_content.starts_with(&cache.raw_content) {
        return rebuild_write_highlight_cache_full(raw_path, file_content);
    }
    // TS: `fileContent.length === cache.rawContent.length`（UTF-16 code unit 长度）。
    if crate::truncate::utf16_len(file_content) == crate::truncate::utf16_len(&cache.raw_content) {
        return Some(cache);
    }

    // `starts_with` 已保证前缀按字节一致，按字节切分与 TS 的 `slice(旧 length)` 等价。
    let delta_raw = &file_content[cache.raw_content.len()..];
    let delta_display = normalize_display_text(delta_raw);
    let delta_normalized = replace_tabs(&delta_display);
    cache.raw_content = file_content.to_string();
    if cache.normalized_lines.is_empty() {
        cache.normalized_lines.push(String::new());
        cache.highlighted_lines.push(String::new());
    }

    let segments: Vec<&str> = delta_normalized.split('\n').collect();
    let last_index = cache.normalized_lines.len() - 1;
    cache.normalized_lines[last_index].push_str(segments[0]);
    cache.highlighted_lines[last_index] =
        highlight_single_line(&cache.normalized_lines[last_index], &cache.lang);
    for segment in &segments[1..] {
        cache.normalized_lines.push((*segment).to_string());
        cache
            .highlighted_lines
            .push(highlight_single_line(segment, &cache.lang));
    }
    refresh_write_highlight_prefix(&mut cache);
    Some(cache)
}

/// `trimTrailingEmptyLines(lines)`。
fn trim_trailing_empty_lines(mut lines: Vec<String>) -> Vec<String> {
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// `args?.file_path ?? args?.path`。
fn pick_path(args: Option<&JsonValue>) -> Option<&JsonValue> {
    let object = args.and_then(JsonValue::as_object)?;
    match object.get("file_path") {
        Some(value) if !value.is_null() => Some(value),
        _ => object.get("path"),
    }
}

/// `formatWriteCall(args, options, theme, cache, cwd)`。
///
/// `content` 经 `str` 后有三种状态：
/// - `None`（类型不是字符串）→ 追加错误文案 `[invalid content arg - expected string]`；
/// - `Some("")`（缺失/`null`/空串）→ 只有标题行；
/// - 非空 → 预览内容：有语言时用 `cache.highlighted_lines`（无缓存则现场高亮），
///   无语言时用 `normalize_display_text` 切行并逐行上 toolOutput 色。
///
/// 预览先裁掉尾部空行再统计行数：折叠最多 10 行，展开显示全部；折叠且超出时追加
/// `... (N more lines, TOTAL total, 键位)` 脚注。`options.is_partial` 在此未使用。
fn format_write_call(
    args: Option<&JsonValue>,
    options: &ToolRenderResultOptions,
    theme: &Theme,
    cache: Option<&WriteHighlightCache>,
    cwd: &str,
) -> String {
    let raw_path = str(pick_path(args));
    let file_content = str(args
        .and_then(JsonValue::as_object)
        .and_then(|object| object.get("content")));
    let path_display = render_tool_path(raw_path.as_deref(), theme, cwd, None);
    let mut text = format!(
        "{} {path_display}",
        theme.fg(ThemeColor::ToolTitle, &theme.bold("write"))
    );

    match file_content {
        None => {
            text.push_str(&format!(
                "\n\n{}",
                theme.fg(ThemeColor::Error, "[invalid content arg - expected string]")
            ));
        }
        Some(file_content) if !file_content.is_empty() => {
            let lang = raw_path
                .as_deref()
                .filter(|path| !path.is_empty())
                .and_then(get_language_from_path);
            let rendered_lines: Vec<String> = match lang {
                Some(_) => match cache {
                    Some(cache) => cache.highlighted_lines.clone(),
                    None => {
                        highlight_code(&replace_tabs(&normalize_display_text(&file_content)), lang)
                    }
                },
                None => normalize_display_text(&file_content)
                    .split('\n')
                    .map(str::to_string)
                    .collect(),
            };
            let lines = trim_trailing_empty_lines(rendered_lines);
            let total_lines = lines.len();
            let max_lines = if options.expanded { lines.len() } else { 10 };
            let display_lines = &lines[..lines.len().min(max_lines)];
            let remaining = lines.len().saturating_sub(max_lines);
            let rendered = display_lines
                .iter()
                .map(|line| match lang {
                    Some(_) => line.clone(),
                    None => theme.fg(ThemeColor::ToolOutput, &replace_tabs(line)),
                })
                .collect::<Vec<_>>()
                .join("\n");
            text.push_str("\n\n");
            text.push_str(&rendered);
            if remaining > 0 {
                text.push_str(&theme.fg(
                    ThemeColor::Muted,
                    &format!("\n... ({remaining} more lines, {total_lines} total,"),
                ));
                text.push(' ');
                text.push_str(&kiana_ui_kit::keybinding_hints::key_hint(
                    "app.tools.expand",
                    "to expand",
                ));
                text.push_str(&theme.fg(ThemeColor::Muted, ")"));
            }
        }
        Some(_) => {}
    }

    text
}

/// `formatWriteResult(result, theme)`：非错误或无文本时返回 `None`。
///
/// 只看 `result.is_error == Some(true)`：把所有文本块用 `\n` 拼接（图片块跳过），
/// 拼接结果为空则返回 `None`，否则前置 `\n` 并整体包上 error 色。
/// `write_renderers` 会先用 `context.is_error` 覆盖该字段，见那里的说明。
fn format_write_result(result: &AgentToolResult<JsonValue>, theme: &Theme) -> Option<String> {
    if result.is_error != Some(true) {
        return None;
    }
    let output = result
        .content
        .iter()
        .filter_map(|block| match block {
            kiana_ai::types::UserContentBlock::Text(text) => Some(text.text.clone()),
            kiana_ai::types::UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if output.is_empty() {
        return None;
    }
    Some(format!("\n{}", theme.fg(ThemeColor::Error, &output)))
}

/// `writeRenderers`。
///
/// 返回 `render_shell: None`、`render_call`/`render_result` 均为 `Some` 的集合，由
/// `crate::write::create_write_tool_definition` 装进工具定义。
///
/// `render_call` 的缓存生命周期：
/// 1. 从 `context.last_component` 下转型取上一帧的 [`WriteCallRenderComponent`]
///    （首帧或类型不符时为 `None`）；
/// 2. `content` 是字符串时：`args_complete` 为真做全量重建，否则在旧缓存上增量更新；
///    `content` 类型错误时丢弃缓存（下一次渲染没有上一帧缓存，只能全量重建）；
/// 3. 用缓存渲染文本，返回携带缓存的新组件。
///
/// `render_result` 不看展开状态：先用 `context.is_error` 覆盖克隆结果的 `is_error`
/// （TS: `{ ...result, isError: context.isError }`，最终以 `context.is_error` 为准），
/// 再交给 `format_write_result`；没有错误或没有文本时返回空的 [`Container`]（TS 会复用
/// `lastComponent` 的 `Container` 并清空，这里每次都新建，渲染结果相同）。
pub fn write_renderers() -> ToolRenderers {
    let render_call: RenderCallFn = Arc::new(|args, theme, context: &ToolRenderContext| {
        let raw_path = str(pick_path(Some(args)));
        let file_content = str(args.as_object().and_then(|object| object.get("content")));
        // 上一帧返回的组件（首帧或类型不符时为 None），缓存从这里取。
        let previous = context
            .last_component
            .as_deref()
            .and_then(Component::as_any)
            .and_then(|any| any.downcast_ref::<WriteCallRenderComponent>());
        // 参数完整时全量重建；流式中尝试增量；content 类型错误时没有缓存。
        let cache = match &file_content {
            Some(file_content) => {
                if context.args_complete {
                    rebuild_write_highlight_cache_full(raw_path.as_deref(), file_content)
                } else {
                    update_write_highlight_cache_incremental(
                        previous.and_then(|component| component.cache.clone()),
                        raw_path.as_deref(),
                        file_content,
                    )
                }
            }
            None => None,
        };
        // format_write_call 只读取 expanded；is_partial 为保持 TS 参数形状而填。
        let options = ToolRenderResultOptions {
            expanded: context.expanded,
            is_partial: context.is_partial,
        };
        let text = format_write_call(
            Some(args),
            &options,
            theme,
            cache.as_ref(),
            &context.cwd.to_string_lossy(),
        );
        Box::new(WriteCallRenderComponent::new(text, cache)) as Box<dyn Component>
    });

    let render_result: RenderResultFn =
        Arc::new(|result, _options, theme, context: &ToolRenderContext| {
            // TS: `formatWriteResult({ ...result, isError: context.isError }, theme)`。
            let mut result = result.clone();
            result.is_error = Some(context.is_error);
            match format_write_result(&result, theme) {
                Some(output) => Box::new(Text::with_padding(output, 0, 0)) as Box<dyn Component>,
                None => Box::new(Container::new()) as Box<dyn Component>,
            }
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
    fn incremental_cache_matches_full_rebuild() {
        let full = rebuild_write_highlight_cache_full(Some("src/main.rs"), "fn main() {}\n")
            .expect("lang");
        // 从空前缀开始逐块追加，最终应与一次性重建一致。
        let mut cache = update_write_highlight_cache_incremental(None, Some("src/main.rs"), "");
        for chunk in ["fn ", "main", "() {}", "\n"] {
            let content = match &cache {
                Some(cache) => format!("{}{chunk}", cache.raw_content),
                None => chunk.to_string(),
            };
            cache = update_write_highlight_cache_incremental(cache, Some("src/main.rs"), &content);
        }
        let cache = cache.expect("cache");
        assert_eq!(cache.normalized_lines, full.normalized_lines);
        assert_eq!(cache.highlighted_lines, full.highlighted_lines);
    }
}
