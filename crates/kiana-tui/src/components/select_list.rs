//! components/select-list.ts。
//!
//! 可选择列表：循环选择、按过滤器过滤、按需滚动、两列（主列 + 描述）布局、鼠标滚轮与点击。
//!
//! ## 负责的边界
//!
//! 本文件只负责三件事：维护“当前选中项 + 过滤结果”，把条目按给定宽度渲染成行，以及把
//! 鼠标/按键翻译成 `on_select`/`on_cancel`/`on_selection_change` 回调。挂载、焦点、布局分配
//! 与滚动容器都不在这里——列表自己只渲染一个以选中项为中心的 `max_visible` 行窗口（贴边
//! 时夹取）。
//!
//! 调用方包括编辑器补全（`components/editor/mod.rs` 的 `create_autocomplete_list`）、扩展
//! 宿主（`kiana-extensions` 的 JS `SelectList` 包装）以及 `kiana-interactive` 的若干选择器
//! （如 `components/theme_selector.rs`）；它们持有列表并负责把回调结果带出去。
//!
//! ## 数据结构与流程
//!
//! - `items` 是全部条目，`filtered_items` 是当前过滤结果。构造时克隆一份，之后只由
//!   `SelectList::set_filter` 重建；与 TS 里 `filteredItems = items`（同一数组引用）不同，
//!   外部没法再原地修改列表内容。
//! - 交互只改 `selected_index` 并调用相应回调（下标类型 `isize`）：空列表按下 ↑ 会越界成 -1
//!   （继续按会更负，见 `handle_input`），所以读取选中项一律走 `get_selected_item`。`render`
//!   不写状态，但每次都会重新计算可见窗口与两列宽度。
//! - 渲染顺序：空列表 → 一行 no-match；否则渲染 `[start, end)` 窗口（有描述且够宽走两列，
//!   否则单列）；窗口没有覆盖全部条目时追加一行 `(选中序号/总数)` 提示。
//!
//! ## 注意事项
//!
//! - `max_visible` 不做校验，0 是合法值（扩展 API 的缺省就是 0）：此时不输出任何条目行，
//!   只可能输出 no-match 或滚动提示行。
//! - 显示文本取 `label`，`label` 为空时回退到 `value`；`description` 先折叠换行再按 JS 的
//!   空白集合 trim，仅由这些空白字符（如 U+FEFF；**不含** U+0085）组成的描述按“没有描述”
//!   处理（见 [`normalize_to_single_line`] 与 `SelectList::render` 里的注释）。
//! - 回调在 `&mut self` 方法执行期间被调用，实现不能反过来借用同一个列表（避免重入借用）。

use std::sync::Arc;

use crate::components::text_component::StyleFn;
use crate::keybindings::get_keybindings;
use crate::text::{trim_js, truncate_to_width, visible_width};
use crate::tui::{
    Component, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType,
};

/// 主列默认宽度（TS `DEFAULT_PRIMARY_COLUMN_WIDTH`）。
///
/// 只在 `min_primary_column_width` 与 `max_primary_column_width` 都未配置时使用。
pub const DEFAULT_PRIMARY_COLUMN_WIDTH: f64 = 32.0;
/// 主列与描述之间的间隔（TS `PRIMARY_COLUMN_GAP`）。
///
/// 固定 2 列：用于算主列“最宽条目”时的加宽量，也是主列文本可用宽度的预留量
/// （`effective_primary_column_width - PRIMARY_COLUMN_GAP`）；它**不等于**实际渲染间距——
/// 间距取 `max(1, 主列总宽 - 已渲染文本宽)`，用于把描述列对齐。
pub const PRIMARY_COLUMN_GAP: isize = 2;
/// 描述列可用的最小宽度（TS `MIN_DESCRIPTION_WIDTH`）。
///
/// 判据是**严格大于**：剩余宽度 `<= MIN_DESCRIPTION_WIDTH` 时放弃描述列，回退到单列渲染。
pub const MIN_DESCRIPTION_WIDTH: f64 = 10.0;

/// 列表条目（TS `SelectItem`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectItem {
    /// 过滤键：`set_filter` 按它的**小写前缀**匹配；`label` 为空时也用它做显示文本。
    pub value: String,
    /// 显示文本；空串回退到 `value`（TS `item.label || item.value`）。
    pub label: String,
    /// 可选描述，只在渲染期使用：先归一化成单行，再决定是否走两列布局。
    pub description: Option<String>,
}

impl SelectItem {
    /// 只要 value 的条目：`label` 也设成 `value`，`description` 为 `None`。
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        SelectItem {
            label: value.clone(),
            value,
            description: None,
        }
    }

    /// 带 label/description 的条目。
    ///
    /// 这里只设置 `value` 与 `label`（`description` 为 `None`）；`label` 为空时渲染会回退到
    /// `value`。
    pub fn with_label(value: impl Into<String>, label: impl Into<String>) -> Self {
        SelectItem {
            value: value.into(),
            label: label.into(),
            description: None,
        }
    }

    /// 带描述（链式调用）。
    ///
    /// 渲染时会先做单行归一化：空串、或只含 JS trim 空白（如 U+FEFF；**不含** U+0085）的
    /// 描述最终按“没有描述”处理，不会启用两列布局；只含 U+0085 时会被当作非空描述
    /// （回归用例见 `crates/kiana-tui/tests/select_list.rs` 的
    /// `description_of_only_next_line_is_kept`）。
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// 主题（TS `SelectListTheme`）：函数字段的 `Clone` struct。
///
/// 每个字段都是“纯文本 → 带 ANSI 的文本”的样式函数；除 `selected_prefix` 外，其余字段都
/// 在 `SelectList::render` / `render_item` 里被调用。
#[derive(Clone)]
pub struct SelectListTheme {
    /// 选中行前缀样式。**当前渲染不使用**：TS 类型里声明了 `selectedPrefix`，但
    /// `spec/tui-components.md` §2.12 注明“声明但渲染中未使用”，本文件的 `render_item`
    /// 也直接写死 `"→ "`；字段保留是因为扩展 API 仍会读写它（见 `kiana-extensions` 的
    /// `SelectListTheme` 转换）。
    pub selected_prefix: StyleFn,
    /// 选中行整行样式（前缀、主列、间距、描述一起套用）。
    pub selected_text: StyleFn,
    /// 未选中行的描述样式：只套在“间距 + 描述”片段上，主列文本保持原样。
    pub description: StyleFn,
    /// 可见窗口没有覆盖全部条目时，那行 `(选中序号/总数)` 的样式。
    pub scroll_info: StyleFn,
    /// 空列表提示 `"  No matching commands"` 的样式。
    pub no_match: StyleFn,
}

impl SelectListTheme {
    /// 恒等主题（不添加 ANSI）。
    pub fn plain() -> Self {
        let identity: StyleFn = Arc::new(|text: &str| text.to_string());
        SelectListTheme {
            selected_prefix: identity.clone(),
            selected_text: identity.clone(),
            description: identity.clone(),
            scroll_info: identity.clone(),
            no_match: identity,
        }
    }
}

/// `truncatePrimary` 的上下文（TS `SelectListTruncatePrimaryContext`）。
///
/// Rust 的回调在 `render` 期间借用列表，因此 `item` 以克隆形式传入（回调不能写回）。
#[derive(Debug, Clone, PartialEq)]
pub struct SelectListTruncatePrimaryContext {
    /// 待截断的显示文本（`label`，为空则 `value`）。
    pub text: String,
    /// 主列可用宽度，随渲染路径不同：两列路径为 `max(1, 主列总宽 -
    /// [`PRIMARY_COLUMN_GAP`])`；单列回退路径为 `width - 前缀宽 - 2`（可为负，此时
    /// `truncate_to_width_f64` 返回空串）。回调的返回值还会被再截到这个宽度。
    pub max_width: f64,
    /// 主列参考宽度：两列路径为夹取后的主列总宽（文本宽 + gap 的预留量）；单列回退路径与
    /// 上面的 `max_width` 相同，不额外包含 gap。
    pub column_width: f64,
    /// 当前条目的克隆（回调只读）。
    pub item: SelectItem,
    /// 当前行是否被选中。
    pub is_selected: bool,
}

/// 条目回调（TS `onSelect`/`onSelectionChange`）。
///
/// 收到的是条目克隆而非引用：回调执行期间列表正被 `&mut` 借用，不能把内部引用交出去。
pub type SelectItemCallback = Box<dyn FnMut(&SelectItem)>;
/// 取消回调（TS `onCancel`）。
pub type SelectCancelCallback = Box<dyn FnMut()>;

/// 主动列截断回调。
///
/// 返回的文本仍会被 `truncate_to_width` 按 `max_width` 再截一次；签名要求 `Send + Sync`
/// （`Arc` 共享给布局选项）。
pub type TruncatePrimaryFn = Arc<dyn Fn(&SelectListTruncatePrimaryContext) -> String + Send + Sync>;

/// 布局选项（TS `SelectListLayoutOptions`）。
#[derive(Clone, Default)]
pub struct SelectListLayoutOptions {
    /// 主列最小宽度：只给 min 或 max 之一时，两者都取该值（固定列宽）。
    pub min_primary_column_width: Option<f64>,
    /// 主列最大宽度；两个都缺省时使用 [`DEFAULT_PRIMARY_COLUMN_WIDTH`]。
    pub max_primary_column_width: Option<f64>,
    /// 自定义主列截断（例如换成 `…` 省略号）；`None` 用 `truncate_to_width`。
    pub truncate_primary: Option<TruncatePrimaryFn>,
}

impl std::fmt::Debug for SelectListLayoutOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelectListLayoutOptions")
            .field("min_primary_column_width", &self.min_primary_column_width)
            .field("max_primary_column_width", &self.max_primary_column_width)
            .field(
                "truncate_primary",
                &self.truncate_primary.as_ref().map(|_| "<fn>"),
            )
            .finish()
    }
}

/// 可选择列表（TS `class SelectList`）。
///
/// `items` / `filtered_items` 都是值拷贝（不是 TS 的同一数组引用）；`selected_index` 是相对
/// `filtered_items` 的下标。三个回调字段公开、构造后由调用方安装，`set_filter` /
/// `set_selected_index` 这类直接设置**不会**触发回调。
pub struct SelectList {
    /// 全部条目（包括当前被过滤掉的）。
    items: Vec<SelectItem>,
    /// 当前过滤结果；渲染与交互都只使用它。
    filtered_items: Vec<SelectItem>,
    /// TS 的 `selectedIndex` 是 number，空列表按下 ↑ 会变成 -1；用 `isize` 保留该行为。
    selected_index: isize,
    /// 上一次 press 命中的 `filtered_items` 下标；click 用它作为“真正要选中的行”，随后清空。
    mouse_pressed_index: Option<isize>,
    /// 可见窗口最大行数；不校验，0 是合法输入（此时窗口为空）。
    max_visible: usize,
    theme: SelectListTheme,
    layout: SelectListLayoutOptions,
    /// 确认选中时调用（Enter 或鼠标 click），参数是条目克隆。
    pub on_select: Option<SelectItemCallback>,
    /// 取消时调用（默认键位 `escape` / `ctrl+c`）。
    pub on_cancel: Option<SelectCancelCallback>,
    /// 方向键（up/down）路径总会调用一次（即使下标未变；空列表下标越界时
    /// `notify_selection_change` 会静默跳过）；滚轮与鼠标 press/click 只在下标变化时调用；
    /// `set_filter`/`set_selected_index` 不触发。
    pub on_selection_change: Option<SelectItemCallback>,
}

impl SelectList {
    /// `constructor(items, maxVisible, theme, layout = {})`。
    ///
    /// `items` 被取走所有权并克隆一份给 `filtered_items`；`max_visible` 原样保存（0 合法）。
    /// 初始选中下标为 0，三个回调都是 `None`。
    pub fn new(
        items: Vec<SelectItem>,
        max_visible: usize,
        theme: SelectListTheme,
        layout: SelectListLayoutOptions,
    ) -> Self {
        SelectList {
            filtered_items: items.clone(),
            items,
            selected_index: 0,
            mouse_pressed_index: None,
            max_visible,
            theme,
            layout,
            on_select: None,
            on_cancel: None,
            on_selection_change: None,
        }
    }

    /// `setFilter(filter)`：按 value 的小写前缀过滤，并重置选择。
    ///
    /// 大小写不敏感（两边都转小写），且每次都从 `items` 全量重算，不在旧结果上二次过滤。
    /// 无论结果是否为空都把 `selected_index` 置 0；不调用 `on_selection_change`，调用方需要
    /// 自己安排后续渲染。
    pub fn set_filter(&mut self, filter: &str) {
        let needle = filter.to_lowercase();
        self.filtered_items = self
            .items
            .iter()
            .filter(|item| item.value.to_lowercase().starts_with(&needle))
            .cloned()
            .collect();
        self.selected_index = 0;
    }

    /// `setSelectedIndex(index)`：夹取到 `[0, len-1]`。
    ///
    /// 先 `min(index, len-1)` 再 `max(0, …)`：空列表时 `len-1 = -1` 会被抬回 0。不触发
    /// 回调，也不改过滤结果。
    pub fn set_selected_index(&mut self, index: isize) {
        self.selected_index =
            isize::max(0, isize::min(index, self.filtered_items.len() as isize - 1));
    }

    /// `getSelectedItem()`。
    ///
    /// 下标为负（空列表按过 ↑）或超出过滤结果时返回 `None`。
    pub fn get_selected_item(&self) -> Option<&SelectItem> {
        if self.selected_index < 0 {
            return None;
        }
        self.filtered_items.get(self.selected_index as usize)
    }

    /// 当前选中下标（TS 的 `selectedIndex`）。
    pub fn selected_index(&self) -> isize {
        self.selected_index
    }

    /// 过滤后的条目。
    pub fn filtered_items(&self) -> &[SelectItem] {
        &self.filtered_items
    }

    /// 全部条目。
    pub fn items(&self) -> &[SelectItem] {
        &self.items
    }

    fn render_item(
        &self,
        item: &SelectItem,
        is_selected: bool,
        width: usize,
        description_single_line: Option<&str>,
        primary_column_width: f64,
    ) -> String {
        // 前缀宽度固定为 2（"→ " 与 "  " 同宽），所以选中状态不会移动后续列。
        let prefix = if is_selected { "→ " } else { "  " };
        let prefix_width = visible_width(prefix);

        // 两列布局：只有存在非空描述且总宽度 > 40 才尝试；下方 remaining_width 不足时同样
        // 会落到单列分支（描述被丢弃，而不是压缩主列）。
        if let Some(description) = description_single_line
            && width > 40
        {
            // 主列宽取配置值与（总宽 - 前缀 - 固定 4 列）中的较小者，再抬到至少 1 列。
            let effective_primary_column_width = f64::max(
                1.0,
                f64::min(
                    primary_column_width,
                    width as f64 - prefix_width as f64 - 4.0,
                ),
            );
            // 主列文本的可用宽度 = 主列总宽 - gap，至少 1 列。
            let max_primary_width = f64::max(
                1.0,
                effective_primary_column_width - PRIMARY_COLUMN_GAP as f64,
            );
            let truncated_value = self.truncate_primary(
                item,
                is_selected,
                max_primary_width,
                effective_primary_column_width,
            );
            let truncated_value_width = visible_width(&truncated_value);
            // TS `" ".repeat(n)`：`repeat` 会把小数截断（ToIntegerOrInfinity）。
            // 间距 = 主列总宽 - 已渲染文本宽（至少 1），用于把描述列对齐到主列右缘；它不是
            // PRIMARY_COLUMN_GAP（默认 32 列主列配 1 列文本时是 31 个空格），GAP 只在计算
            // 主列文本可用宽度和最宽条目时使用。`max(1, …)` 兜底保证描述不会紧贴主列。
            let spacing_count = f64::max(
                1.0,
                effective_primary_column_width - truncated_value_width as f64,
            ) as usize;
            let spacing = " ".repeat(spacing_count);
            let description_start =
                prefix_width as f64 + truncated_value_width as f64 + spacing.len() as f64;
            let remaining_width = width as f64 - description_start - 2.0; // -2 安全边距

            // 严格大于 MIN_DESCRIPTION_WIDTH 才画描述；否则离开本分支，走下面的单列渲染。
            if remaining_width > MIN_DESCRIPTION_WIDTH {
                let truncated_desc = truncate_to_width_f64(description, remaining_width, "");
                if is_selected {
                    // 选中行整体套 selected_text（前缀与描述也一起着色）。
                    return (self.theme.selected_text)(&format!(
                        "{prefix}{truncated_value}{spacing}{truncated_desc}"
                    ));
                }

                // 未选中：主列不着色，只有“间距 + 描述”套 description 样式。
                let desc_text = (self.theme.description)(&format!("{spacing}{truncated_desc}"));
                return format!("{prefix}{truncated_value}{desc_text}");
            }
        }

        // 单列回退：没有描述、宽度 <= 40、或剩余宽度不足以放描述列。
        let max_width = width as f64 - prefix_width as f64 - 2.0;
        let truncated_value = self.truncate_primary(item, is_selected, max_width, max_width);
        if is_selected {
            return (self.theme.selected_text)(&format!("{prefix}{truncated_value}"));
        }

        format!("{prefix}{truncated_value}")
    }

    fn truncate_primary(
        &self,
        item: &SelectItem,
        is_selected: bool,
        max_width: f64,
        column_width: f64,
    ) -> String {
        let display_value = self.get_display_value(item);
        let truncated_value = match &self.layout.truncate_primary {
            Some(truncate_primary) => truncate_primary(&SelectListTruncatePrimaryContext {
                text: display_value.clone(),
                max_width,
                column_width,
                item: item.clone(),
                is_selected,
            }),
            None => truncate_to_width_f64(&display_value, max_width, ""),
        };

        // 兜底：自定义回调即使返回超宽文本，也要再截一次；`max_width <= 0`（窄视口）时
        // truncate_to_width_f64 直接返回空串。
        truncate_to_width_f64(&truncated_value, max_width, "")
    }

    fn get_display_value(&self, item: &SelectItem) -> String {
        // TS `item.label || item.value`：空 label 回退到 value。
        if item.label.is_empty() {
            item.value.clone()
        } else {
            item.label.clone()
        }
    }

    fn get_primary_column_width(&self) -> f64 {
        let (min, max) = self.get_primary_column_bounds();
        // 取所有**过滤后**条目里最宽的显示文本 + gap，再夹到配置的上下界；空列表时
        // widest 为 0，clamp 后得到下界。
        let widest_primary = self.filtered_items.iter().fold(0.0f64, |widest, item| {
            f64::max(
                widest,
                visible_width(&self.get_display_value(item)) as f64 + PRIMARY_COLUMN_GAP as f64,
            )
        });

        clamp(widest_primary, min, max)
    }

    fn get_primary_column_bounds(&self) -> (f64, f64) {
        // 只配置了一个边界时，两个原始值都取它 → 固定列宽；都没配置时都取默认值。
        let raw_min = self
            .layout
            .min_primary_column_width
            .or(self.layout.max_primary_column_width)
            .unwrap_or(DEFAULT_PRIMARY_COLUMN_WIDTH);
        let raw_max = self
            .layout
            .max_primary_column_width
            .or(self.layout.min_primary_column_width)
            .unwrap_or(DEFAULT_PRIMARY_COLUMN_WIDTH);

        // 先 min/max 交换保证下界 <= 上界（配置写反也不 panic），再把两者都抬到至少 1。
        (
            f64::max(1.0, f64::min(raw_min, raw_max)),
            f64::max(1.0, f64::max(raw_min, raw_max)),
        )
    }

    fn get_visible_range(&self) -> (usize, usize) {
        let length = self.filtered_items.len() as isize;
        let half = (self.max_visible / 2) as isize;
        // 选中项尽量居中：起点 = 选中下标 - half；再夹到 [0, length - max_visible]。
        // `length - max_visible` 可能为负，外层 max(0, …) 会兜住。
        let start_index = isize::max(
            0,
            isize::min(
                self.selected_index - half,
                length - self.max_visible as isize,
            ),
        );
        // 终点再按实际长度截断；max_visible = 0 时恒有 start == end（窗口为空）。
        (
            start_index as usize,
            usize::min(
                (start_index + self.max_visible as isize) as usize,
                self.filtered_items.len(),
            ),
        )
    }

    fn notify_selection_change(&mut self) {
        // 只在“当前下标能取到条目”时回调：空列表按 ↑ 后 index = -1，或过滤结果变短导致
        // 下标越界，都静默跳过。
        let index = self.selected_index;
        if index < 0 {
            return;
        }
        let Some(item) = self.filtered_items.get(index as usize) else {
            return;
        };
        if let Some(on_selection_change) = &mut self.on_selection_change {
            on_selection_change(item);
        }
    }
}

impl Component for SelectList {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();

        // 空列表只有一行提示（文案逐字同 TS），且不做宽度截断——超宽时由上层裁剪。
        if self.filtered_items.is_empty() {
            lines.push((self.theme.no_match)("  No matching commands"));
            return lines;
        }

        // 列宽按“全部过滤结果”计算一次，循环内只按行渲染。
        let primary_column_width = self.get_primary_column_width();
        let (start_index, end_index) = self.get_visible_range();

        for i in start_index..end_index {
            let Some(item) = self.filtered_items.get(i) else {
                continue;
            };
            let is_selected = i as isize == self.selected_index;
            // TS：`item.description ? normalizeToSingleLine(item.description) : undefined`。
            // `normalizeToSingleLine` 会 trim，因此纯空白/换行的描述归一化为 `""`，在
            // `descriptionSingleLine && width > 40` 里按 JS 的真值语义视为**没有描述**。
            let description_single_line = item
                .description
                .as_deref()
                .map(normalize_to_single_line)
                .filter(|description| !description.is_empty());
            lines.push(self.render_item(
                item,
                is_selected,
                width,
                description_single_line.as_deref(),
                primary_column_width,
            ));
        }

        // 可见窗口没覆盖全部条目时，追加一行 “  (序号/总数)” 提示；序号是 1-based。
        // 宽度先减 2（`saturating_sub` 防止窄视口下溢）再截断。
        if start_index > 0 || end_index < self.filtered_items.len() {
            let scroll_text = format!(
                "  ({}/{})",
                self.selected_index + 1,
                self.filtered_items.len()
            );
            lines.push((self.theme.scroll_info)(&truncate_to_width(
                &scroll_text,
                width.saturating_sub(2),
                "",
                false,
            )));
        }

        lines
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        // 空列表直接返回 None：滚轮、hover、点击都不处理。
        if self.filtered_items.is_empty() {
            return None;
        }
        if event.event_type == TuiMouseEventType::Wheel
            && let Some(wheel_delta) = event.wheel_delta
            && wheel_delta != 0.0
        {
            // 只取方向（负 = 上滚，其余 = 下滚），幅度忽略；选中下标夹在 [0, len-1]，
            // 到边界后继续滚不报错、也不回绕。
            let delta = if wheel_delta < 0.0 { -1 } else { 1 };
            let previous_index = self.selected_index;
            self.selected_index = isize::max(
                0,
                isize::min(
                    self.filtered_items.len() as isize - 1,
                    self.selected_index + delta,
                ),
            );
            if self.selected_index != previous_index {
                self.notify_selection_change();
            }
            // 滚轮始终返回 handled，`render` 只在选中项真的变化时为 true。
            return Some(TuiMouseEventResult {
                handled: Some(true),
                render: Some(self.selected_index != previous_index),
                ..Default::default()
            });
        }
        // Hover 不改变选择：可见范围以选中项为中心。
        if event.button != TuiMouseButton::Left
            || (event.event_type != TuiMouseEventType::Press
                && event.event_type != TuiMouseEventType::Click)
        {
            return None;
        }
        // 行号是相对**可见窗口**的：event.y = 0 对应 start_index 那一行。
        let (start_index, end_index) = self.get_visible_range();
        let item_index = start_index as isize + event.y;
        if item_index < start_index as isize || item_index >= end_index as isize {
            return None;
        }

        if event.event_type == TuiMouseEventType::Press {
            // 记录按下的行供 click 使用，并立即把选中项移到该行；focus 请求键盘焦点。
            self.mouse_pressed_index = Some(item_index);
            if self.selected_index != item_index {
                self.selected_index = item_index;
                self.notify_selection_change();
            }
            return Some(TuiMouseEventResult {
                handled: Some(true),
                focus: Some(true),
                ..Default::default()
            });
        }
        if event.event_type == TuiMouseEventType::Click {
            // 优先用 press 记录的行（TS `mousePressedIndex ?? itemIndex`）；没有 press 记录时
            // 用当前行。随后清空记录，避免影响下一次点击。
            let clicked_index = self.mouse_pressed_index.unwrap_or(item_index);
            self.mouse_pressed_index = None;
            let changed = self.selected_index != clicked_index;
            self.selected_index = clicked_index;
            if changed {
                self.notify_selection_change();
            }
            // click 同时是一次“确认”：把克隆的条目交给 on_select（回调不能持有列表引用）。
            let selected_item = if self.selected_index >= 0 {
                self.filtered_items
                    .get(self.selected_index as usize)
                    .cloned()
            } else {
                None
            };
            if let Some(selected_item) = selected_item
                && let Some(on_select) = &mut self.on_select
            {
                on_select(&selected_item);
            }
            return Some(TuiMouseEventResult {
                handled: Some(true),
                ..Default::default()
            });
        }
        None
    }

    fn handle_input(&mut self, key_data: &str) {
        // 键位通过全局 KeybindingsManager 解析（默认 up/down/enter/escape|ctrl+c），
        // 这里只关心动作 id，不直接比较按键文本。
        let kb = get_keybindings();
        if kb.matches(key_data, "tui.select.up") {
            // 上箭头：在顶部时回绕到底部。
            // 空列表时 len-1 == -1：第一次按 ↑ 得到 -1，之后继续按会继续递减——选中项始终
            // 取不到（get_selected_item 返回 None），TS 同款不做夹取。
            self.selected_index = if self.selected_index == 0 {
                self.filtered_items.len() as isize - 1
            } else {
                self.selected_index - 1
            };
            // 注意：与滚轮/鼠标路径不同，方向键路径无条件回调一次（单条目列表按 ↓ 时
            // 下标没变也会通知）。
            self.notify_selection_change();
        } else if kb.matches(key_data, "tui.select.down") {
            // 下箭头：在底部时回绕到顶部。
            self.selected_index = if self.selected_index == self.filtered_items.len() as isize - 1 {
                0
            } else {
                self.selected_index + 1
            };
            self.notify_selection_change();
        } else if kb.matches(key_data, "tui.select.confirm") {
            // Enter：确认当前条目；下标无效或回调未安装时什么也不做。
            let selected_item = if self.selected_index >= 0 {
                self.filtered_items
                    .get(self.selected_index as usize)
                    .cloned()
            } else {
                None
            };
            if let Some(selected_item) = selected_item
                && let Some(on_select) = &mut self.on_select
            {
                on_select(&selected_item);
            }
        } else if kb.matches(key_data, "tui.select.cancel")
            && let Some(on_cancel) = &mut self.on_cancel
        {
            // Escape / ctrl+c：取消（不会清空列表或选中项，语义由调用方决定）。
            on_cancel();
        }
    }
}

/// `normalizeToSingleLine`：把换行折叠成一个空格并 trim。
///
/// 连续的 `\r`/`\n` 只产生一个空格；其它字符（含普通空格与制表符）原样保留。末尾的 trim
/// 用 [`trim_js`]（JS `String.prototype.trim` 的空白集合），因此 U+FEFF 会被去掉、U+0085
/// 不会——这决定了两列布局是否启用（回归用例见 `crates/kiana-tui/tests/select_list.rs`）。
pub fn normalize_to_single_line(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_break = false;
    for ch in text.chars() {
        if ch == '\r' || ch == '\n' {
            if !in_break {
                result.push(' ');
                in_break = true;
            }
        } else {
            in_break = false;
            result.push(ch);
        }
    }
    trim_js(&result).to_string()
}

/// `clamp(value, min, max) = Math.max(min, Math.min(value, max))`。
fn clamp(value: f64, min: f64, max: f64) -> f64 {
    f64::max(min, f64::min(value, max))
}

/// TS 允许 `maxWidth` 为小数/负数；`maxWidth <= 0` → `""`。
///
/// 有限的正值向下取整后交给 [`truncate_to_width`]；`NaN` 与 `+∞` 按“不限制”处理
/// （`is_finite` 为假 → `usize::MAX`；`NaN <= 0.0` 也为假，所以不会命中空串分支）。
/// `-∞` 命中 `max_width <= 0.0`，返回空串。
fn truncate_to_width_f64(text: &str, max_width: f64, ellipsis: &str) -> String {
    if max_width <= 0.0 {
        return String::new();
    }
    let width = if max_width.is_finite() {
        max_width.floor() as usize
    } else {
        usize::MAX
    };
    truncate_to_width(text, width, ellipsis, false)
}
