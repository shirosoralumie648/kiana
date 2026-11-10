//! visual-truncate.ts（VisualLinePreview）。
//!
//! 把文本截断到「可视行数」（考虑自动换行）。工具渲染器与 bash-execution 共用。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/visual-truncate.ts`。
//!
//! 与 TS 的差异：
//! * `keep` 的字符串字面量 `"start" | "end"` → [`Keep`] 枚举；
//! * TS `maxVisualLines` 是 number（负数走 `slice` 的负索引语义）。Rust 用 `usize`；
//!   语义等价的可观察特例（`max === 0` 时 `slice(-0)` 返回**全部**行）仍然保留。
//! * `formatHint` 的 `number` 参数 → `usize`（skippedCount 为非负整数）。
//!
//! # 算法与边界
//!
//! 先把整段文本包进一个临时 [`Text`]（`padding_x` 原样传递、`padding_y = 0`），借它的
//! ANSI 感知换行得到**可视行**：一个逻辑行可能已经折成多行，且每行都被补齐到 `width`。
//! 然后按 `keep` 截取前 N 行或后 N 行。截断的是屏幕上的行数而不是 `\n` 的个数——调用方
//! （MCP 结果、bash 输出等）经常是一整行超长 JSON/日志，按逻辑行截断没有意义。
//!
//! 边界：
//!
//! * 空文本直接返回空结果（不构造 `Text`）；只含空白的文本经 `Text` 渲染同样得到 0 行。
//! * `max_visual_lines == 0` 且 `keep = End` 时，JS 的 `slice(-0)` 等于 `slice(0)`：返回
//!   **全部**行，但 `skipped_count` 仍等于总行数；`keep = Start` 时 `slice(0, 0)` 返回空。
//!   这是刻意保留的 TS 可观察行为。
//! * 截断分支中 `skipped_count = 总行数 - max_visual_lines`；未截断时（`len <= max`）
//!   返回 0 —— 该分支不计算差值，因此不会下溢。
//!
//! # 调用方
//!
//! `kiana-tools` 的 bash 渲染器、`kiana-builtin-extensions` 的 MCP 工具结果与 codemode
//! 渲染器、`kiana-interactive` 的 bash 执行组件。折叠行数由各调用方自己的常量决定
//! （例如 `BASH_PREVIEW_LINES`、`OUTPUT_PREVIEW_LINES`），本模块不提供默认值。

use kiana_tui::components::text_component::Text;
use kiana_tui::text::truncate_to_width;
use kiana_tui::tui::Component;

/// 保留哪一端（TS `keep: "start" | "end"`，默认 `"end"`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Keep {
    /// 保留前 N 行；hint 放在保留行之后。
    Start,
    /// 保留后 N 行；hint 放在保留行之前（默认）。
    #[default]
    End,
}

/// 截断结果（TS `VisualTruncateResult`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VisualTruncateResult {
    /// 要显示的可视行。
    pub visual_lines: Vec<String>,
    /// 被隐藏的可视行数。
    pub skipped_count: usize,
}

/// TS `truncateToVisualLines(text, maxVisualLines, width, paddingX = 0, keep = "end")`。
///
/// `Text(text, padding_x, 0)` 的 `paddingY` 固定为 0；空文本直接返回空结果（不构造 `Text`）。
///
/// 参数与返回值：
///
/// * `text`：可含 ANSI 转义与 `\n`，按词换行由内部 `Text` 处理；
/// * `max_visual_lines`：保留的可视行数上限；`0` 的语义见模块文档（TS `slice(-0)` 特例）；
/// * `width`：渲染宽度（列），决定换行与每行补白；
/// * `padding_x`：左右留白列数，原样传给内部 `Text`，结果每行可见宽度仍为 `width`；
/// * `keep`：保留开头还是结尾；
/// * 返回的 `visual_lines` 是**已着色、已补白**的最终行，可直接渲染；未截断时就是
///   `Text::render` 的全部输出，`skipped_count` 为被隐藏的可视行数。
pub fn truncate_to_visual_lines(
    text: &str,
    max_visual_lines: usize,
    width: usize,
    padding_x: usize,
    keep: Keep,
) -> VisualTruncateResult {
    if text.is_empty() {
        return VisualTruncateResult::default();
    }

    let mut temp_text = Text::with_padding(text, padding_x, 0);
    let all_visual_lines = temp_text.render(width);

    if all_visual_lines.len() <= max_visual_lines {
        return VisualTruncateResult {
            visual_lines: all_visual_lines,
            skipped_count: 0,
        };
    }

    let truncated_lines = match keep {
        Keep::Start => all_visual_lines[..max_visual_lines].to_vec(),
        // JS `slice(-0)` 等于 `slice(0)`，返回整个数组。
        Keep::End if max_visual_lines == 0 => all_visual_lines.clone(),
        Keep::End => all_visual_lines[all_visual_lines.len() - max_visual_lines..].to_vec(),
    };
    let skipped_count = all_visual_lines.len() - max_visual_lines;

    VisualTruncateResult {
        visual_lines: truncated_lines,
        skipped_count,
    }
}

/// `VisualLinePreviewOptions`（字段与上游接口逐字对应）。
///
/// 所有字段按值捕获，组件不校验 `max_visual_lines`。`format_hint` 只在真的隐藏了行时调用，
/// 参数是被隐藏的可视行数；返回的字符串随后会被 `truncate_to_width(..., "...", false)`
/// 裁到渲染宽度。
pub struct VisualLinePreviewOptions {
    /// 已上色的文本；可含 `\n`。
    pub text: String,
    /// 最多显示的可视行数。
    pub max_visual_lines: usize,
    /// 保留哪一端；hint 放在保留段之前（end）还是之后（start）。
    pub keep: Keep,
    /// 给定隐藏行数的提示行（已上色）。
    pub format_hint: Box<dyn Fn(usize) -> String>,
}

/// 折叠的工具输出（TS `class VisualLinePreview implements Component`）。
///
/// 按宽度缓存渲染结果（转录区里每个结果每帧都会渲染）。文本在构造时按值捕获；
/// 如果宿主改了文本，必须调用 [`Component::invalidate`]，否则同宽度下继续返回旧缓存。
///
/// 缓存键只有渲染宽度：同一宽度下重复 `render` 不会重新截断，也不会再次调用
/// `format_hint`。`options` 字段是私有的，替换选项要用 [`VisualLinePreview::set_options`]
/// （它会清空缓存），不要指望在原地改 `VisualLinePreviewOptions` 能生效。
pub struct VisualLinePreview {
    options: VisualLinePreviewOptions,
    cached_width: Option<usize>,
    cached_lines: Option<Vec<String>>,
}

impl VisualLinePreview {
    /// TS `constructor(options)`。
    ///
    /// 构造时不渲染、不调用 `format_hint`；缓存为空，首次 `render` 才计算。
    pub fn new(options: VisualLinePreviewOptions) -> Self {
        VisualLinePreview {
            options,
            cached_width: None,
            cached_lines: None,
        }
    }

    /// 替换选项并清空缓存（TS 里调用方会重建组件；这里提供等价入口）。
    ///
    /// 下一次 `render` 用新选项重算；旧缓存不会保留任何行。
    pub fn set_options(&mut self, options: VisualLinePreviewOptions) {
        self.options = options;
        self.invalidate();
    }

    /// 当前文本。
    pub fn text(&self) -> &str {
        &self.options.text
    }
}

impl Component for VisualLinePreview {
    /// 命中（同宽度且已有缓存）时直接克隆缓存行；否则重算截断、按需生成提示行并写回缓存。
    fn render(&mut self, width: usize) -> Vec<String> {
        if self.cached_lines.is_none() || self.cached_width != Some(width) {
            // `padding_x` 固定传 0：组件不在内部加左右留白，需要留白的调用方在
            // `options.text` 里自行处理。
            let preview = truncate_to_visual_lines(
                &self.options.text,
                self.options.max_visual_lines,
                width,
                0,
                self.options.keep,
            );
            let lines = preview.visual_lines;
            // 只有真的隐藏了行才生成提示；提示先按渲染宽度截断（省略号 "...", 不补白），
            // 再按 keep 插到保留段之前或之后。`max = 0` 且 keep = End 时 skipped_count
            // 等于总行数、却返回全部行，于是提示会声称隐藏了所有行——TS 的既有行为。
            let cached_lines = if preview.skipped_count > 0 {
                let hint = truncate_to_width(
                    &(self.options.format_hint)(preview.skipped_count),
                    width,
                    "...",
                    false,
                );
                match self.options.keep {
                    Keep::Start => {
                        let mut result = lines;
                        result.push(hint);
                        result
                    }
                    Keep::End => {
                        let mut result = Vec::with_capacity(lines.len() + 1);
                        result.push(hint);
                        result.extend(lines);
                        result
                    }
                }
            } else {
                lines
            };
            self.cached_lines = Some(cached_lines);
            self.cached_width = Some(width);
        }
        self.cached_lines.clone().unwrap_or_default()
    }

    fn handles_input(&self) -> bool {
        // TS `VisualLinePreview` 没有 `handleInput`。
        false
    }

    fn invalidate(&mut self) {
        self.cached_width = None;
        self.cached_lines = None;
    }
}
