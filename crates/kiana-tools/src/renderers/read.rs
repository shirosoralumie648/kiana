//! core/tools/renderers/read.ts。
//!
//! `read` 工具的 TUI 渲染：调用行（含 `skill`/`docs`/`resource` 紧凑分类）与结果行
//! （折叠时不渲染、展开时高亮、10 行预览、逐字截断脚注）。
//!
//! 与 TS 的差异仅在「组件复用」：Rust 的 [`ToolRenderContext::last_component`] 是
//! 借用而非可变引用，无法像 TS 那样原地 `setText`；这里每次返回新的 [`Text`]，
//! 渲染输出完全相同（TS 的复用纯粹是分配优化）。
//!
//! `file_path` 优先于 `path`（nullish coalescing，`""` 不会回落），兼容旧参数名。
//!
//! # 在系统中的位置
//!
//! 由 `crate::read::create_read_tool_definition` 通过 [`read_renderers`] 装进工具定义的
//! `render_call` / `render_result` 槽，`render_shell` 为 `None`（默认外壳）。宿主
//! （如 `kiana-interactive` 的 `ToolExecutionComponent`）每次重新渲染时把原始参数、
//! 展开状态和上一帧组件（[`ToolRenderContext::last_component`]）交回；本模块
//! **不使用**上一帧组件。
//!
//! # 调用行的执行流程
//!
//! 1. 用 `file_path ?? path` 取路径；非字符串 → `[invalid arg]`，缺失/`null`/`""` → `...`。
//! 2. 折叠时先尝试紧凑分类：`SKILL.md` → 父目录名；包根的 `README.md`、包内 `docs/`、
//!    `examples/` 下的文件 → `read docs <相对包根路径>`；`AGENTS.md`/`CLAUDE.md`
//!    等固定名单 → `read resource <相对 cwd 路径，不在 cwd 内时为绝对路径>`。命中后
//!    追加展开提示 ` (<键位> to expand)`。
//! 3. 未命中或已展开时显示 `read <路径>`；`offset`/`limit` 可解析时追加 `:N`/`:N-M`
//!    （warning 色）。
//!
//! # 结果行的执行流程
//!
//! 折叠且非错误 → 空串（不占行）。否则：拼接文本块（`get_text_output`）→ 路径有
//! 已知语言且非错误时整段高亮 → 去掉尾部空行 → 折叠视图最多 10 行，其余显示
//! `... (N more lines, <键位> to expand)` → 最后按 `details.truncation` 追加截断脚注。
//!
//! # 注意事项
//!
//! - `docs` 分类的包根是 `kiana_core::config::get_readme_path()` 的目录部分，取决于
//!   `KIANA_PACKAGE_DIR` 或可执行文件位置；用户 cwd 下的同名 `docs/` 目录不属于该分类。
//!   分类只是展示层判断，不检查被读取的文件是否存在。
//! - 分类用的路径解析是 `resolve_to_cwd`（纯词法），不包含 `read` 执行路径的 macOS
//!   命名变体回退。
//! - 本渲染器不读取 `context.output_pad`，调用行与结果行都返回零内边距的 [`Text`]；
//!   非 self-render 的缩进由 `ToolExecutionComponent` 按其 `outputPad` 统一应用。

use std::path::{Component as PathComponent, Path, PathBuf};

use kiana_agent_core::AgentToolResult;
use kiana_ai::types::JsonValue;
use kiana_ext_api::tool_types::{
    RenderCallFn, RenderResultFn, ToolRenderContext, ToolRenderResultOptions, ToolRenderers,
};
use kiana_theme::theme::{Theme, get_language_from_path, highlight_code};
use kiana_theme::theme_tokens::ThemeColor;
use kiana_tui::components::text_component::Text;
use kiana_tui::tui::Component;

use super::details;
use crate::path_utils::resolve_to_cwd;
use crate::render_utils::{
    format_read_line_range, get_text_output, render_tool_path, replace_tabs, str,
};

/// `COMPACT_RESOURCE_FILE_NAMES`。
///
/// 命中时调用行折叠成 `read resource <相对 cwd 或绝对路径>`。比较是**区分大小写**的
/// 逐字文件名匹配，所以列表里同时放了 `.md` 与 `.MD` 两种写法；不按后缀或内容判断。
const COMPACT_RESOURCE_FILE_NAMES: [&str; 5] = [
    "AGENTS.override.md",
    "AGENTS.md",
    "AGENTS.MD",
    "CLAUDE.md",
    "CLAUDE.MD",
];

/// 紧凑分类。TS: `CompactReadClassification`。
///
/// `label` 是已经格式化好的展示文本（不含颜色）：`skill` 是 `SKILL.md` 的父目录名，
/// `docs` 是相对包根的路径，`resource` 是相对 cwd（或绝对）的路径；后两者统一用 `/`
/// 分隔。
#[derive(Debug, Clone, PartialEq, Eq)]
struct CompactReadClassification {
    kind: CompactReadKind,
    label: String,
}

/// 分类种类。
///
/// 只影响调用行的文案与配色：`docs`/`resource` 渲染成 `read <kind> <label>`，
/// `skill` 渲染成 `[skill] <label>`；`as_str` 的返回值直接进入文案。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompactReadKind {
    Docs,
    Resource,
    Skill,
}

impl CompactReadKind {
    fn as_str(self) -> &'static str {
        match self {
            CompactReadKind::Docs => "docs",
            CompactReadKind::Resource => "resource",
            CompactReadKind::Skill => "skill",
        }
    }
}

/// JS `args?.file_path ?? args?.path`。
///
/// `file_path` 存在且不是 `null` 时优先（`""` 也算存在，不会回落到 `path`）；
/// 否则取 `path`。两者都缺省时返回 `None`，交给 `str` 变成空串。
fn pick_path(args: Option<&JsonValue>) -> Option<&JsonValue> {
    let object = args.and_then(JsonValue::as_object)?;
    match object.get("file_path") {
        Some(value) if !value.is_null() => Some(value),
        _ => object.get("path"),
    }
}

/// JS `args?.[key]`：`args` 不是对象（含 `null`）时返回 `None`；键存在但值为
/// `null` 时返回 `Some(null)`，两者语义不同。
fn arg<'a>(args: Option<&'a JsonValue>, key: &str) -> Option<&'a JsonValue> {
    args.and_then(JsonValue::as_object)?.get(key)
}

/// JS `path.basename`。
fn js_basename(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// JS `path.dirname`（相对路径没有父目录时返回 `.`）。
fn js_dirname(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// JS `path.relative(from, to)`（两者都按已解析的绝对路径处理）。
///
/// 实现只做组件级词法比较：公共前缀之后，`from` 侧每层补 `..`，`to` 侧取剩余组件，
/// 用平台分隔符连接。不访问文件系统；`..` 只作为普通组件参与比较，不会先被折叠。
fn js_relative(from: &Path, to: &Path) -> String {
    let from_components: Vec<PathComponent<'_>> = from.components().collect();
    let to_components: Vec<PathComponent<'_>> = to.components().collect();
    let mut common = 0usize;
    while common < from_components.len()
        && common < to_components.len()
        && from_components[common] == to_components[common]
    {
        common += 1;
    }
    let mut parts: Vec<String> = Vec::new();
    for _ in common..from_components.len() {
        parts.push("..".to_string());
    }
    for component in &to_components[common..] {
        parts.push(component.as_os_str().to_string_lossy().into_owned());
    }
    parts.join(std::path::MAIN_SEPARATOR_STR)
}

/// `toPosixPath(filePath)`：把平台分隔符换成 `/`。
fn to_posix_path(file_path: &str) -> String {
    file_path.replace(std::path::MAIN_SEPARATOR, "/")
}

/// `styledReadLineRange(args, theme)`：`:offset-limit` 用 warning 色。
///
/// `offset`/`limit` 都缺失或为 `null` 时不显示范围；仅 `limit` 缺失/null 时只输出
/// `:N`（起点从第 1 行起）。任一存在的值无法解析成有限数字时，`format_read_line_range`
/// 返回空串，这里也不着色，整段直接消失（两者都缺省时即 `read a.txt`）。
fn styled_read_line_range(args: Option<&JsonValue>, theme: &Theme) -> String {
    let range = format_read_line_range(arg(args, "offset"), arg(args, "limit"));
    if range.is_empty() {
        String::new()
    } else {
        theme.fg(ThemeColor::Warning, &range)
    }
}

/// `formatReadCall(args, theme, cwd)`。
///
/// 普通（非紧凑）调用行：`read <路径><行号范围>`。路径展示交给 `render_tool_path`：
/// 非字符串 → `[invalid arg]`，缺失/`null`/空串 → `...`，否则按 `~` 压缩，终端支持
/// 超链接时再加 OSC 8 文件链接。
fn format_read_call(args: Option<&JsonValue>, theme: &Theme, cwd: &str) -> String {
    let path_display = render_tool_path(str(pick_path(args)).as_deref(), theme, cwd, None);
    format!(
        "{} {path_display}{}",
        theme.fg(ThemeColor::ToolTitle, &theme.bold("read")),
        styled_read_line_range(args, theme)
    )
}

/// `trimTrailingEmptyLines(lines)`。
fn trim_trailing_empty_lines(mut lines: Vec<String>) -> Vec<String> {
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// `getKianaDocsClassification(absolutePath)`。
///
/// 判断文件是否位于**安装包内**的文档位置：相对包根（`get_readme_path()` 的目录）的
/// 路径非空、不在包外、且是 `README.md` 本身或以 `docs/`、`examples/` 开头时返回
/// `docs` 分类，标签就是这段相对路径（先转成 `/` 分隔）。
///
/// 相对路径为空（包根本身）、等于 `..`、以 `..<分隔符>` 开头，或相对计算后仍是
/// 绝对路径（`is_absolute` 兜底）时返回 `None`。
fn get_kiana_docs_classification(absolute_path: &Path) -> Option<CompactReadClassification> {
    let package_root = js_dirname(&kiana_core::config::get_readme_path());
    let relative_path = js_relative(&package_root, absolute_path);
    if relative_path.is_empty()
        || relative_path == ".."
        || relative_path.starts_with(&format!("..{}", std::path::MAIN_SEPARATOR))
        || Path::new(&relative_path).is_absolute()
    {
        return None;
    }

    let label = to_posix_path(&relative_path);
    if label == "README.md" || label.starts_with("docs/") || label.starts_with("examples/") {
        return Some(CompactReadClassification {
            kind: CompactReadKind::Docs,
            label,
        });
    }
    None
}

/// `getCompactReadClassification(args, cwd)`。
///
/// 分类顺序固定，先命中者优先：`SKILL.md` → 包内文档 → 资源文件名。因此包内
/// `docs/` 下的 `SKILL.md` 会显示成 `[skill]` 而不是 `read docs`。
///
/// 路径先经 `resolve_to_cwd` 做纯词法解析（不检查存在性）；原始路径为空串时直接
/// 返回 `None`。`skill` 的标签取父目录名，父目录名为空（如 `/SKILL.md`）时退回
/// 文件名本身。
fn get_compact_read_classification(
    args: Option<&JsonValue>,
    cwd: &str,
) -> Option<CompactReadClassification> {
    let raw_path = str(pick_path(args))?;
    if raw_path.is_empty() {
        return None;
    }

    let absolute_path = resolve_to_cwd(&raw_path, cwd);
    let file_name = js_basename(&absolute_path);
    if file_name == "SKILL.md" {
        let parent_name = js_basename(&js_dirname(&absolute_path));
        let label = if parent_name.is_empty() {
            file_name
        } else {
            parent_name
        };
        return Some(CompactReadClassification {
            kind: CompactReadKind::Skill,
            label,
        });
    }

    if let Some(classification) = get_kiana_docs_classification(&absolute_path) {
        return Some(classification);
    }

    if COMPACT_RESOURCE_FILE_NAMES.contains(&file_name.as_str()) {
        return Some(CompactReadClassification {
            kind: CompactReadKind::Resource,
            label: kiana_core::paths::format_path_relative_to_cwd_or_absolute(
                &absolute_path.to_string_lossy(),
                cwd,
            ),
        });
    }

    None
}

/// `formatCompactReadCall(classification, args, theme)`。
///
/// 三种分类共用结尾的 dim 展开提示 ` (键位 to expand)`。`skill` 用
/// customMessageLabel 色包住裸 SGR 加粗的 `[skill]`，标签用 customMessageText 色；
/// `docs`/`resource` 用 toolTitle 加粗的 `read <kind>` 加 accent 色标签。
fn format_compact_read_call(
    classification: &CompactReadClassification,
    args: Option<&JsonValue>,
    theme: &Theme,
) -> String {
    let expand_hint = theme.fg(
        ThemeColor::Dim,
        &format!(
            " ({} to expand)",
            kiana_ui_kit::keybinding_hints::key_text("app.tools.expand")
        ),
    );
    if classification.kind == CompactReadKind::Skill {
        return format!(
            "{}{}{}{}",
            theme.fg(ThemeColor::CustomMessageLabel, "\x1b[1m[skill]\x1b[22m "),
            theme.fg(ThemeColor::CustomMessageText, &classification.label),
            styled_read_line_range(args, theme),
            expand_hint,
        );
    }

    format!(
        "{} {}{}{}",
        theme.fg(
            ThemeColor::ToolTitle,
            &theme.bold(&format!("read {}", classification.kind.as_str())),
        ),
        theme.fg(ThemeColor::Accent, &classification.label),
        styled_read_line_range(args, theme),
        expand_hint,
    )
}

/// 结果里的文本输出（`getTextOutput`）。
fn result_text(result: &AgentToolResult<JsonValue>, show_images: bool) -> String {
    get_text_output(Some(result), show_images)
}

/// `formatReadResult(args, result, options, theme, showImages, _cwd, isError)`。
///
/// `is_error` 来自 `ToolRenderContext::is_error`（不是 `result.is_error`）：错误结果
/// 即使折叠也显示，且**不做语法高亮**（TS 在同一处用 `!isError` 条件跳过语言检测）。
///
/// 阶段：
/// 1. `!expanded && !is_error` → 空串；
/// 2. `get_text_output` 拼接文本块（去 ANSI、按需追加图片占位）；
/// 3. 路径有已知语言且非错误 → 整段 `highlight_code`，否则按 `\n` 切行；
/// 4. 去掉尾部空行后，折叠视图最多 10 行，剩余行数写入展开提示；
/// 5. `details.truncation.truncated` 为真时，按 `firstLineExceedsLimit` →
///    `truncatedBy == "lines"` → 字节截断 的顺序追加一条 warning 脚注。
///
/// 需要显示结果时（展开或错误）输出以 `\n` 开头（紧跟调用行）；折叠且非错误时返回
/// 空串，没有前导换行。显示分支里即使内容为空也保留这个前导换行。`details` 走
/// `crate::renderers::details` 的逐字段宽松读取：`truncated` 为真时，其它字段缺失只
/// 会退回默认值或 `undefined`，不会让脚注整体消失。`read` 工具只在发生截断时填写
/// `details`，正常读取不会出现脚注。
fn format_read_result(
    args: Option<&JsonValue>,
    result: &AgentToolResult<JsonValue>,
    options: &ToolRenderResultOptions,
    theme: &Theme,
    show_images: bool,
    is_error: bool,
) -> String {
    if !options.expanded && !is_error {
        return String::new();
    }

    let raw_path = str(pick_path(args));
    let output = result_text(result, show_images);
    let lang = if !is_error {
        raw_path
            .as_deref()
            .filter(|path| !path.is_empty())
            .and_then(get_language_from_path)
    } else {
        None
    };
    let rendered_lines = match lang {
        Some(language) => highlight_code(&replace_tabs(&output), Some(language)),
        None => output.split('\n').map(str::to_string).collect(),
    };
    let lines = trim_trailing_empty_lines(rendered_lines);
    let max_lines = if options.expanded { lines.len() } else { 10 };
    let display_lines = lines.iter().take(max_lines);
    let remaining = lines.len().saturating_sub(max_lines);
    let mut text = format!(
        "\n{}",
        display_lines
            .map(|line| match lang {
                Some(_) => replace_tabs(line),
                None => theme.fg(ThemeColor::ToolOutput, &replace_tabs(line)),
            })
            .collect::<Vec<_>>()
            .join("\n")
    );
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

    // 脚注优先级：首行超限 > 按行截断 > 按字节截断；三者文案逐字对齐 TS。
    let details = Some(&result.details);
    if details::truncation_truncated(details) {
        if details::truncation_truthy(details, "firstLineExceedsLimit") {
            text.push_str(&format!(
                "\n{}",
                theme.fg(
                    ThemeColor::Warning,
                    &format!(
                        "[First line exceeds {} limit]",
                        details::truncation_max_bytes_size(details)
                    ),
                )
            ));
        } else if details::truncated_by_lines(details) {
            text.push_str(&format!(
                "\n{}",
                theme.fg(
                    ThemeColor::Warning,
                    &format!(
                        "[Truncated: showing {} of {} lines ({} line limit)]",
                        details::truncation_text(details, "outputLines"),
                        details::truncation_text(details, "totalLines"),
                        details::truncation_max_lines_text(details)
                    ),
                )
            ));
        } else {
            text.push_str(&format!(
                "\n{}",
                theme.fg(
                    ThemeColor::Warning,
                    &format!(
                        "[Truncated: {} lines shown ({} limit)]",
                        details::truncation_text(details, "outputLines"),
                        details::truncation_max_bytes_size(details)
                    ),
                )
            ));
        }
    }
    text
}

/// `readRenderers`。
///
/// 返回 `render_shell: None`、`render_call`/`render_result` 均为 `Some` 的渲染器集合，
/// 由 `crate::read::create_read_tool_definition` 装进工具定义。
///
/// - `render_call`：`context.expanded` 为假时先尝试紧凑分类，否则走普通 `read <路径>`；
///   每次调用都新建零内边距的 [`Text`]，不读取 `context.last_component`。
/// - `render_result`：参数取 `context.args`（与调用行共享的原始 JSON），错误标志取
///   `context.is_error`，其余语义见 `format_read_result`。
///
/// 两个闭包都不保存跨帧状态，同一工具行的多次渲染互不影响。
pub fn read_renderers() -> ToolRenderers {
    let render_call: RenderCallFn = std::sync::Arc::new(|args, theme, context| {
        // 展开后不做紧凑分类（展开态需要完整调用行）；cwd 转成字符串供纯词法解析使用。
        let classification = if !context.expanded {
            get_compact_read_classification(Some(args), &context.cwd.to_string_lossy())
        } else {
            None
        };
        let text = match &classification {
            Some(classification) => format_compact_read_call(classification, Some(args), theme),
            None => format_read_call(Some(args), theme, &context.cwd.to_string_lossy()),
        };
        Box::new(Text::with_padding(text, 0, 0)) as Box<dyn Component>
    });

    let render_result: RenderResultFn =
        std::sync::Arc::new(|result, options, theme, context: &ToolRenderContext| {
            let text = format_read_result(
                Some(&context.args),
                result,
                options,
                theme,
                context.show_images,
                context.is_error,
            );
            Box::new(Text::with_padding(text, 0, 0)) as Box<dyn Component>
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
    fn js_relative_matches_node() {
        assert_eq!(js_relative(Path::new("/a/b"), Path::new("/a/b")), "");
        assert_eq!(
            js_relative(Path::new("/a/b"), Path::new("/a/c/d")),
            "../c/d"
        );
        assert_eq!(
            js_relative(Path::new("/a/b"), Path::new("/x/y")),
            "../../x/y"
        );
        assert_eq!(js_relative(Path::new("/a"), Path::new("/a/b/c")), "b/c");
    }

    #[test]
    fn basename_and_dirname_match_node() {
        assert_eq!(js_basename(Path::new("/a/b/SKILL.md")), "SKILL.md");
        assert_eq!(js_basename(Path::new("/")), "");
        assert_eq!(js_dirname(Path::new("/a/b")), PathBuf::from("/a"));
        assert_eq!(js_dirname(Path::new("/a")), PathBuf::from("/"));
        assert_eq!(js_dirname(Path::new("SKILL.md")), PathBuf::from("."));
    }
}
