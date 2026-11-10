//! components/settings-list.ts。
//!
//! 设置列表：左列标签、右列当前值、可循环取值或打开子菜单、可选模糊搜索。
//!
//! ## 子菜单 `done()` 的同步语义
//!
//! TS 的 `done(selectedValue?, options?)` 是闭包，直接改 `SettingsList` 的字段并调用
//! `onChange` / `closeSubmenu()`——也就是说 `onChange` 在子菜单的 `handleInput` 内部、
//! 在 `done()` 返回**之前**同步执行（`settings-list.ts:264-308`）。
//!
//! Rust 的 `done` 是 `'static` 回调，不能借用宿主。这里把列表的可变状态放进
//! `Rc<RefCell<SettingsListState>>`，回调持有同一个 `Rc`，因此仍在调用点同步执行
//! `setItemValue` → `onChange` → `closeSubmenu`。子菜单组件本身用
//! `Rc<RefCell<Box<dyn Component>>>` 保活：`done()` 里把 `submenuComponent` 置 `None`
//! 只丢弃列表的引用，组件正在执行的 `handleInput` 仍持有另一个 `Rc`（与 TS 的 GC 语义
//! 相同，避免在方法调用中途释放 `self`）。
//!
//! ## 职责与调用方式
//!
//! [`SettingsList`] 是一个普通 [`crate::tui::Component`]：宿主（`kiana-interactive` 的
//! `/settings` 面板与 `kiana-extensions` 的 JS `SettingsList` 绑定）持有它，并在渲染/
//! 输入循环里转调 `render`、`handle_input`、`handle_mouse`、`invalidate`。
//! 它不订阅终端事件、不创建线程或定时器，所有交互都由宿主驱动。
//!
//! ## 渲染结构
//!
//! 主列表在有条目的情况下每帧从上到下输出：搜索框（启用搜索时占一行）与一个空行、
//! 至多 `max_visible` 行条目、滚动计数（仅当窗口没有覆盖全部条目）、选中项说明（若有，
//! 前面加一个空行）、一个空行与提示行。`max_visible` 只约束条目行，不约束说明与提示；
//! 空列表会提前返回，不输出滚动计数与说明。
//!
//! ## 状态共享与借用
//!
//! 与子菜单 `done` 回调共享的列表状态（`items`、`filtered_items`、`selected_index`、
//! `on_change` 等）放在 `Rc<RefCell<SettingsListState>>` 里，因为 `done` 是 `'static`
//! 回调，必须与列表共享同一份状态；搜索框（`search_input`）与按下行记录
//! （`mouse_pressed_index`）仍留在 [`SettingsList`] 结构体上，不与 `done` 共享。
//! `items` 与 `filtered_items` 是两批 `SettingItem` 克隆（TS 里是同一批对象引用），
//! 所以写当前值时要同时更新两边。
//! `render`、`handle_mouse`、`handle_input`、`activate_item` 在调用用户代码（主题函数、
//! `on_change`、子菜单）之前都会先克隆所需数据并释放 `RefCell` 借用，避免用户代码
//! 重入列表方法时触发双重借用 panic。
//!
//! ## 注意事项
//!
//! - 搜索开启时，`filtered_items` 只在收到未匹配键绑定的输入后由 `apply_filter` 重算；
//!   `items` 的顺序不变。
//! - 搜索框为空时，空格是“切换取值”快捷键；搜索框非空时，空格作为查询字符进入搜索框
//!   （`tests/settings_list.rs` 锁定这两种行为）。
//! - 子菜单打开期间，渲染、按键与鼠标全部转交子菜单；关闭后输入回到主列表。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use crate::components::input::Input;
use crate::components::text_component::StyleFn;
use crate::fuzzy::fuzzy_filter;
use crate::keybindings::get_keybindings;
use crate::text::{truncate_to_width, visible_width, wrap_text_with_ansi};
use crate::tui::{
    Component, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType,
};

/// `done` 的选项参数（TS `{ navigateTo?: string }`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsNavigateOptions {
    /// 关闭子菜单后要选中的设置项 `id`。
    ///
    /// `None` 或空串都表示不跳转（`done` 回调会过滤空串，对应 TS 的真值判断）。
    pub navigate_to: Option<String>,
}

/// 值变化回调（TS `onChange: (id: string, newValue: string) => void`）。
///
/// 参数是条目 id 与新值。调用路径只有两条：循环取值（Enter/空格/鼠标点击最终都走
/// `activate_item` 的取值分支）与子菜单 `done(Some(value))`。循环取值即使算出的新值与
/// 旧值相同（例如只有一个候选值）也会调用——循环取值总视为一次变更；
/// [`SettingsList::update_value`] 与 `select_item` 不会触发它。回调在触发它的列表方法
/// 内部同步执行。
pub type SettingChangeCallback = Box<dyn FnMut(&str, &str)>;
/// 取消回调（TS `onCancel: () => void`）。
///
/// 主列表收到 `tui.select.cancel` 时同步调用。子菜单打开期间 Esc 由子菜单自己处理，
/// 不会直达这里。
pub type SettingCancelCallback = Box<dyn FnMut()>;
/// 带 selected 参数的主题函数。
///
/// 对应 TS `label`/`value` 的签名：第一个参数是待渲染文本，第二个参数表示该行是否
/// 被选中；返回值原样拼进行内，因此可以携带 ANSI 序列。
pub type SettingTextFn = Arc<dyn Fn(&str, bool) -> String + Send + Sync>;

/// 子菜单关闭时的回调（TS `done(selectedValue?, options?)`）。
///
/// - `Some(value)`：把当前值写回条目并触发 [`SettingChangeCallback`]；`None` 不写值、
///   不触发 `onChange`，只关闭子菜单。
/// - `options.navigate_to` 非空：关闭后选中该 id 并自动激活（可能再次打开它的子菜单）；
///   即使 `selected_value` 是 `None`，这条跳转仍然执行。
///
/// 回调由子菜单在自己的 `handle_input`/鼠标处理里调用，执行是同步的（见模块文档）。
pub type SettingsDoneCallback = Box<dyn FnMut(Option<String>, Option<SettingsNavigateOptions>)>;

/// 子菜单工厂（TS `submenu?: (currentValue, done) => Component`）。
///
/// 第一个参数是打开时的条目当前值（供子菜单预选）；第二个参数用于关闭并回传结果。
/// 工厂在 `activate_item` 内同步调用，允许在工厂内部立刻调用 `done`（此时列表会先
/// 看到“尚未打开”的状态，与 TS 的赋值顺序一致）。
pub type SubmenuFactory = Rc<dyn Fn(&str, SettingsDoneCallback) -> Box<dyn Component>>;

/// 设置项（TS `SettingItem`）。
#[derive(Clone)]
pub struct SettingItem {
    /// 唯一标识。
    pub id: String,
    /// 左侧显示标签。
    pub label: String,
    /// 选中时显示在列表下方的说明。
    pub description: Option<String>,
    /// 右侧显示的当前值。
    pub current_value: String,
    /// 提供时 Enter/Space 在这些值之间循环。
    pub values: Option<Vec<String>>,
    /// 提供时 Enter 打开该子菜单。
    pub submenu: Option<SubmenuFactory>,
}

impl std::fmt::Debug for SettingItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingItem")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("description", &self.description)
            .field("current_value", &self.current_value)
            .field("values", &self.values)
            .field("submenu", &self.submenu.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

impl SettingItem {
    /// 只有 id/label/currentValue 的条目。
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        current_value: impl Into<String>,
    ) -> Self {
        SettingItem {
            id: id.into(),
            label: label.into(),
            description: None,
            current_value: current_value.into(),
            values: None,
            submenu: None,
        }
    }
}

/// 主题（TS `SettingsListTheme`）。
#[derive(Clone)]
pub struct SettingsListTheme {
    /// 左侧标签：文本已按封顶 36 的最长标签宽度补空格，第二个参数表示该行是否选中。
    pub label: SettingTextFn,
    /// 右侧当前值：文本已截断到可用列宽，第二个参数表示该行是否选中。
    pub value: SettingTextFn,
    /// 选中项说明：每个已换行、带两空格缩进的片段调用一次。
    pub description: StyleFn,
    /// 选中行前缀（字符串，不是函数）；非选中行固定用两个空格占位。
    pub cursor: String,
    /// 提示行：空状态、无匹配、滚动计数与底部快捷键提示都走它。
    pub hint: StyleFn,
}

impl SettingsListTheme {
    /// 恒等主题（不添加 ANSI）。
    pub fn plain() -> Self {
        let identity: StyleFn = Arc::new(|text: &str| text.to_string());
        let identity_selected: SettingTextFn =
            Arc::new(|text: &str, _selected: bool| text.to_string());
        SettingsListTheme {
            label: identity_selected.clone(),
            value: identity_selected,
            description: identity.clone(),
            cursor: "> ".to_string(),
            hint: identity,
        }
    }
}

/// 选项（TS `SettingsListOptions`）。
#[derive(Debug, Clone, Default)]
pub struct SettingsListOptions {
    /// 是否启用模糊搜索；`None` 按 `false` 处理（TS `enableSearch ?? false`）。
    pub enable_search: Option<bool>,
}

/// 列表的可变状态（与子菜单的 `done()` 回调共享）。
///
/// `items` 是完整条目，`filtered_items` 是搜索开启时的显示子集；两者都是独立克隆，
/// 任何写当前值的路径都必须同步两者（见 `set_item_value`）。`selected_index` 的下标
/// 空间是**显示列表**（搜索开启时为 `filtered_items`，否则为 `items`）。`on_change`
/// 放在 `Rc<RefCell<_>>` 里，`done` 闭包通过共享的 `state` 取到它并回调宿主。
struct SettingsListState {
    /// 全部条目（保持传入顺序）。
    items: Vec<SettingItem>,
    /// 当前过滤结果；搜索关闭时不会用于渲染，但 `set_item_value` 仍会同步写入它。
    filtered_items: Vec<SettingItem>,
    /// 显示列表中的选中下标。
    selected_index: isize,
    /// 与 `done` 闭包共享的值变化回调。
    on_change: Rc<RefCell<SettingChangeCallback>>,
    search_enabled: bool,
    /// 当前打开的子菜单。用 `Rc` 保活：`done()` 可能在组件自己的 `handle_input` 里把它
    /// 从列表移除，调用栈上的另一份 `Rc` 保证方法返回前对象仍然存活。
    submenu_component: Option<Rc<RefCell<Box<dyn Component>>>>,
    /// 打开子菜单时选中的显示列表下标；关闭时用于恢复选择。
    submenu_item_index: Option<isize>,
    /// `done` 请求的跳转目标，由 `close_submenu` 消费。
    navigate_after_close: Option<String>,
}

/// 设置列表（TS `class SettingsList`）。
///
/// 主题、`on_cancel`、`max_visible`、搜索框与按下行记录留在结构体上；条目与选择等
/// 列表状态在 `SettingsListState` 中，与子菜单回调共享。`search_input` 只在构造时启用
/// 搜索才存在。
pub struct SettingsList {
    /// 与子菜单 `done` 共享的列表状态。
    state: Rc<RefCell<SettingsListState>>,
    /// 颜色、前缀与提示的渲染函数。
    theme: SettingsListTheme,
    /// 主列表一次最多显示的条目行数（不含搜索框、说明与提示行）。
    max_visible: usize,
    /// 主列表收到取消键时的回调（TS `onCancel`）。
    on_cancel: SettingCancelCallback,
    /// 搜索框；仅在 `enable_search` 为真时创建。
    search_input: Option<Input>,
    /// 上一次 press 命中的显示下标，click 优先用它（TS `mousePressedIndex`）。
    mouse_pressed_index: Option<isize>,
}

impl SettingsList {
    /// `constructor(items, maxVisible, theme, onChange, onCancel, options = {})`。
    ///
    /// `items` 会被克隆成完整列表与初始过滤结果两份；`max_visible` 语义见结构体字段
    /// 说明；`options.enable_search` 为真时创建内部搜索框。构造本身不校验空列表。
    pub fn new(
        items: Vec<SettingItem>,
        max_visible: usize,
        theme: SettingsListTheme,
        on_change: SettingChangeCallback,
        on_cancel: SettingCancelCallback,
        options: SettingsListOptions,
    ) -> Self {
        let search_enabled = options.enable_search.unwrap_or(false);
        let state = SettingsListState {
            filtered_items: items.clone(),
            items,
            selected_index: 0,
            on_change: Rc::new(RefCell::new(on_change)),
            search_enabled,
            submenu_component: None,
            submenu_item_index: None,
            navigate_after_close: None,
        };
        SettingsList {
            state: Rc::new(RefCell::new(state)),
            theme,
            max_visible,
            on_cancel,
            search_input: if search_enabled {
                Some(Input::default())
            } else {
                None
            },
            mouse_pressed_index: None,
        }
    }

    /// `updateValue(id, newValue)`：只修改值，不触发 `onChange`。
    ///
    /// 同时写入 `items` 与 `filtered_items` 中的克隆；id 不存在时无操作。
    pub fn update_value(&mut self, id: &str, new_value: &str) {
        self.state.borrow_mut().set_item_value(id, new_value);
    }

    /// `selectItem(id)`：把选择移到指定 id（未找到则无操作）。
    ///
    /// 搜索开启时只在当前过滤结果里查找（TS 用 `filteredItems`），否则在全部条目里查找。
    pub fn select_item(&mut self, id: &str) {
        self.state.borrow_mut().select_item(id);
    }

    /// 当前选中项下标（TS `selectedIndex`）。
    ///
    /// 下标空间是显示列表：搜索开启时为过滤结果的下标，否则为 `items` 的下标。
    pub fn selected_index(&self) -> isize {
        self.state.borrow().selected_index
    }

    /// 是否启用搜索。
    ///
    /// 构造后不变（来自 `options.enable_search`）。
    pub fn search_enabled(&self) -> bool {
        self.state.borrow().search_enabled
    }

    fn render_row(
        &self,
        item: &SettingItem,
        is_selected: bool,
        width: usize,
        max_label_width: usize,
    ) -> String {
        // 选中行用主题 cursor，其余行用两空格占位。普通行前缀宽 2，选中行是
        // `visible_width(cursor)`，所以只有 cursor 可见宽度为 2 时两行的值列才对得齐
        // （默认 cursor `"> "` 正好 2 列）；主题传入更宽/更窄的 cursor 时选中行会与
        // 普通行错位。
        let prefix = if is_selected {
            self.theme.cursor.clone()
        } else {
            "  ".to_string()
        };
        let prefix_width = visible_width(&prefix);

        // 补齐 label 以便值列对齐。
        let label_padded = format!(
            "{}{}",
            item.label,
            " ".repeat(max_label_width.saturating_sub(visible_width(&item.label)))
        );
        let label_text = (self.theme.label)(&label_padded, is_selected);

        // 计算值列可用宽度。
        // 末尾额外预留 2 列（TS `width - usedWidth - 2`）；Rust 的 `usize` 不能为负，
        // 因此先按 `isize` 计算再夹到 0。
        let separator = "  ";
        let used_width = prefix_width + max_label_width + visible_width(separator);
        let value_max_width = (width as isize - used_width as isize - 2).max(0) as usize;

        let value_text = (self.theme.value)(
            &truncate_to_width(&item.current_value, value_max_width, "", false),
            is_selected,
        );

        // TS `truncateToWidth(..., width)` 用默认省略号 `"..."`（值列与滚动计数显式传 `""`）。
        truncate_to_width(
            &format!("{prefix}{label_text}{separator}{value_text}"),
            width,
            "...",
            false,
        )
    }

    /// 追加“空行 + 提示行”（TS `addHintLine`）：提示文本取决于搜索是否开启，
    /// 并按 `width` 截断（默认省略号）。
    fn add_hint_line(&self, lines: &mut Vec<String>, width: usize, search_enabled: bool) {
        lines.push(String::new());
        lines.push(truncate_to_width(
            &(self.theme.hint)(if search_enabled {
                "  Type to search · Enter/Space to change · Esc to cancel"
            } else {
                "  Enter/Space to change · Esc to cancel"
            }),
            width,
            "...",
            false,
        ));
    }

    /// 渲染主列表（TS `renderMainList`）。
    ///
    /// 子菜单打开时不会走到这里（`Component::render` 先转发给子菜单）。条目行、
    /// 滚动计数与提示行都按显示列宽计算并交给 `truncate_to_width` 截断；说明按
    /// `width - 4` 换行。`No settings available` 状态行与 TS 一样不截断。
    fn render_main_list(&mut self, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();

        // 搜索框占一行，下面再留一个空行；鼠标命中测试用同样的偏移（row_offset = 2）。
        if self.search_enabled()
            && let Some(search_input) = &mut self.search_input
        {
            lines.extend(search_input.render(width));
            lines.push(String::new());
        }

        // 状态克隆成快照，渲染期间不持有 `RefCell` 借用（主题函数是用户代码）。
        let (items_empty, display_items, selected_index, search_enabled, max_label_width) = {
            let state = self.state.borrow();
            // 标签宽度取**全部**条目（TS 用 `this.items`），并封顶 36 列：过滤时值列
            // 不会因可见标签变短而左右跳动。
            let max_label_width = state
                .items
                .iter()
                .map(|item| visible_width(&item.label))
                .max()
                .unwrap_or(0)
                .min(36);
            (
                state.items.is_empty(),
                state.get_display_items().to_vec(),
                state.selected_index,
                state.search_enabled,
                max_label_width,
            )
        };

        // 条目本身为空：只渲染空状态（TS 在未启用搜索时连提示行都不加）。
        if items_empty {
            lines.push((self.theme.hint)("  No settings available"));
            if search_enabled {
                self.add_hint_line(&mut lines, width, search_enabled);
            }
            return lines;
        }

        // 过滤后为空：显示无匹配提示，并保留底部快捷键提示。
        let display_len = display_items.len();
        if display_len == 0 {
            lines.push(truncate_to_width(
                &(self.theme.hint)("  No matching settings"),
                width,
                "...",
                false,
            ));
            self.add_hint_line(&mut lines, width, search_enabled);
            return lines;
        }

        // 只渲染窗口内的条目；窗口以选中项为中心，具体规则见 `get_visible_range`。
        let (start_index, end_index) =
            get_visible_range(selected_index, self.max_visible, display_len);

        for i in start_index..end_index {
            let Some(item) = display_items.get(i) else {
                continue;
            };
            let is_selected = i as isize == selected_index;
            lines.push(self.render_row(item, is_selected, width, max_label_width));
        }

        // 滚动计数只在窗口没有覆盖全部条目时出现，分母是显示列表长度。
        if start_index > 0 || end_index < display_len {
            let scroll_text = format!("  ({}/{})", selected_index + 1, display_len);
            lines.push((self.theme.hint)(&truncate_to_width(
                &scroll_text,
                width.saturating_sub(2),
                "",
                false,
            )));
        }

        // 选中项的说明。
        // 空说明不占行；换行宽度为 width - 4（两侧各留 2 空格缩进）。
        if selected_index >= 0 {
            let description = display_items
                .get(selected_index as usize)
                .and_then(|item| item.description.as_deref())
                .filter(|description| !description.is_empty())
                .map(str::to_string);
            if let Some(description) = description {
                lines.push(String::new());
                let wrapped_desc = wrap_text_with_ansi(&description, width.saturating_sub(4));
                for line in wrapped_desc {
                    lines.push((self.theme.description)(&format!("  {line}")));
                }
            }
        }

        self.add_hint_line(&mut lines, width, search_enabled);
        lines
    }
}

impl SettingsListState {
    /// 把当前值写入 `items` 与 `filtered_items` 两份克隆；不调用 `onChange`，id 不存在时
    /// 无操作。`update_value`、循环取值与子菜单 `done` 都经由这里改值。
    fn set_item_value(&mut self, id: &str, new_value: &str) {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
            item.current_value = new_value.to_string();
        }
        // TS 的 filteredItems 与 items 是同一批对象引用；Rust 侧是克隆，需同步写。
        if let Some(item) = self.filtered_items.iter_mut().find(|item| item.id == id) {
            item.current_value = new_value.to_string();
        }
    }

    /// 当前用于渲染与命中测试的条目列表：搜索开启时是过滤结果，否则是全部条目。
    fn get_display_items(&self) -> &[SettingItem] {
        if self.search_enabled {
            &self.filtered_items
        } else {
            &self.items
        }
    }

    /// 显示列表中的第 `index` 项；越界返回 `None`。
    fn display_item(&self, index: usize) -> Option<&SettingItem> {
        self.get_display_items().get(index)
    }

    /// `selectItem(id)` 的实现：搜索开启时在过滤结果里查找，否则在全部条目里查找；
    /// 未找到时保持原选择。
    fn select_item(&mut self, id: &str) {
        let index = if self.search_enabled {
            self.filtered_items.iter().position(|item| item.id == id)
        } else {
            self.items.iter().position(|item| item.id == id)
        };
        if let Some(index) = index {
            self.selected_index = index as isize;
        }
    }

    /// 以 `label` 做模糊过滤并把选择重置到第一项（TS `applyFilter`）。
    ///
    /// 每次过滤都从 `items` 重新克隆一批条目；过滤结果里的 `current_value` 之后由
    /// `set_item_value` 与原始列表同步。
    fn apply_filter(&mut self, query: &str) {
        self.filtered_items = fuzzy_filter(self.items.clone(), query, |item| item.label.clone());
        self.selected_index = 0;
    }

    /// `activateItem()`。`state` 是共享引用，`done()` 回调持有同一个 `Rc`，因此子菜单在
    /// 自己的 `handleInput` 里调用 `done()` 时**同步**走完与 TS 相同的步骤。
    fn activate_item(state: &Rc<RefCell<Self>>) {
        // 先克隆激活所需的字段再释放借用，随后才调用用户提供的子菜单工厂 / onChange。
        // 分支优先级：有 submenu 就打开子菜单，否则才在 values 之间循环。
        let Some((item_id, current_value, submenu, values, selected_index)) = ({
            let state = state.borrow();
            if state.selected_index < 0 {
                return;
            }
            state
                .display_item(state.selected_index as usize)
                .map(|item| {
                    (
                        item.id.clone(),
                        item.current_value.clone(),
                        item.submenu.clone(),
                        item.values.clone(),
                        state.selected_index,
                    )
                })
        }) else {
            return;
        };

        if let Some(submenu) = submenu {
            // 打开子菜单，把当前值传进去以便预选。
            state.borrow_mut().submenu_item_index = Some(selected_index);
            let state_for_done = Rc::clone(state);
            let done_item_id = item_id.clone();
            let done: SettingsDoneCallback = Box::new(move |selected_value, options| {
                if let Some(selected_value) = selected_value {
                    state_for_done
                        .borrow_mut()
                        .set_item_value(&done_item_id, &selected_value);
                    notify_change(&state_for_done, &done_item_id, &selected_value);
                }
                // TS `if (options?.navigateTo)`：空串是 falsy，不设置。
                if let Some(navigate_to) = options
                    .and_then(|options| options.navigate_to)
                    .filter(|id| !id.is_empty())
                {
                    state_for_done.borrow_mut().navigate_after_close = Some(navigate_to);
                }
                close_submenu(&state_for_done);
            });
            // TS 先设 `submenuItemIndex`、再调用工厂、最后赋值 `submenuComponent`。
            // 保持同样顺序：工厂若同步调用 `done()`，`closeSubmenu` 看到的是"尚未打开"。
            let component = submenu(&current_value, done);
            state.borrow_mut().submenu_component = Some(Rc::new(RefCell::new(component)));
        } else if let Some(values) = values
            && !values.is_empty()
        {
            // 在值之间循环；当前值不在列表中时从第一项开始。
            let current_index = values.iter().position(|value| value == &current_value);
            let next_index = match current_index {
                Some(index) => (index + 1) % values.len(),
                None => 0,
            };
            let new_value = values[next_index].clone();
            state.borrow_mut().set_item_value(&item_id, &new_value);
            notify_change(state, &item_id, &new_value);
        }
    }
}

/// `onChange(id, newValue)`：克隆回调句柄后**释放状态借用**再调用，避免用户回调里重入
/// 列表方法时触发 `RefCell` 双重借用。
fn notify_change(state: &Rc<RefCell<SettingsListState>>, id: &str, new_value: &str) {
    let callback = Rc::clone(&state.borrow().on_change);
    (callback.borrow_mut())(id, new_value);
}

/// `closeSubmenu()`。
///
/// 两个互斥分支：
/// 1. `done` 提供了非空 `navigate_to`：清空 opener 下标，关闭借用后选中目标项并调用
///    `activate_item`（目标若有子菜单会立刻再次打开）。
/// 2. 没有跳转：把选择恢复到打开子菜单的那一项（`submenu_item_index`）。
///
/// 子菜单引用先被清空；但调用方栈上通常还有一份 `Rc`，所以子菜单对象不会在自己的
/// 方法执行中途被释放。
///
/// 边界：搜索开启且目标 id 不在当前过滤结果里时，`select_item` 不改变选择，随后的
/// `activate_item` 会作用于原来的选中项（与 TS 的 `selectItem` 实现一致）。
fn close_submenu(state: &Rc<RefCell<SettingsListState>>) {
    let navigate_to = {
        let mut state = state.borrow_mut();
        state.submenu_component = None;
        match state.navigate_after_close.take() {
            Some(id) => {
                // 有跳转目标：旧选择不再恢复，opener 下标一并清空。
                state.submenu_item_index = None;
                Some(id)
            }
            None => {
                if let Some(index) = state.submenu_item_index.take() {
                    // 恢复到打开子菜单的那一项。
                    state.selected_index = index;
                }
                None
            }
        }
    };
    if let Some(id) = navigate_to {
        // 借用已释放：select_item 与 activate_item 都会再次借用同一个 RefCell。
        state.borrow_mut().select_item(&id);
        // 自动打开目标项的子菜单。
        SettingsListState::activate_item(state);
    }
}

/// `getVisibleRange(displayItems)`。
///
/// 计算以选中项为中心的可视窗口。起点为
/// `max(0, min(selected_index - max_visible / 2, display_len - max_visible))`（整数除法
/// 等价 JS 的 `Math.floor`）；终点是起点加 `max_visible` 后不超过 `display_len`。
/// 返回半开区间 `[start_index, end_index)`；列表不足一屏时得到 `0..display_len`。
fn get_visible_range(
    selected_index: isize,
    max_visible: usize,
    display_len: usize,
) -> (usize, usize) {
    let length = display_len as isize;
    let half = (max_visible / 2) as isize;
    let start_index = isize::max(
        0,
        isize::min(selected_index - half, length - max_visible as isize),
    );
    (
        start_index as usize,
        usize::min((start_index + max_visible as isize) as usize, display_len),
    )
}

impl Component for SettingsList {
    fn render(&mut self, width: usize) -> Vec<String> {
        // 子菜单激活时渲染它（TS：`if (this.submenuComponent) return ...render(width)`）。
        // 克隆 `Rc` 后释放状态借用：子菜单渲染期间 `done()` 可能改列表状态。
        let submenu = self
            .state
            .borrow()
            .submenu_component
            .as_ref()
            .map(Rc::clone);
        if let Some(submenu) = submenu {
            return submenu.borrow_mut().render(width);
        }
        self.render_main_list(width)
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        // 子菜单优先：鼠标事件全部转交，并把返回结果的 focus 强制置真
        // （TS `{ ...result, focus: true }`）。先克隆 `Rc` 再释放状态借用。
        let submenu = self
            .state
            .borrow()
            .submenu_component
            .as_ref()
            .map(Rc::clone);
        if let Some(submenu) = submenu {
            let result = submenu.borrow_mut().handle_mouse(event);
            return result.map(|mut result| {
                result.focus = Some(true);
                result
            });
        }

        // 搜索模式下行 0 是搜索框、行 1 是它下面的分隔空行；这两行不属于条目。
        let search_enabled = self.search_enabled();
        if search_enabled && let Some(search_input) = &mut self.search_input {
            if event.y == 0 {
                let result = search_input.handle_mouse(event);
                return result.map(|mut result| {
                    result.focus = Some(true);
                    result
                });
            }
            if event.y == 1 {
                return None;
            }
        }

        let display_len = self.state.borrow().get_display_items().len();
        if display_len == 0 {
            return None;
        }
        // 滚轮只把选择移动一行并夹在显示列表内；内容本身不滚动，窗口跟随选中项。
        // `wheel_delta == 0` 不产生事件（TS 的 `event.wheelDelta` 真值判断）。
        if event.event_type == TuiMouseEventType::Wheel
            && let Some(wheel_delta) = event.wheel_delta
            && wheel_delta != 0.0
        {
            let delta = if wheel_delta < 0.0 { -1 } else { 1 };
            let (previous_index, selected_index) = {
                let mut state = self.state.borrow_mut();
                let previous_index = state.selected_index;
                state.selected_index = isize::max(
                    0,
                    isize::min(display_len as isize - 1, state.selected_index + delta),
                );
                (previous_index, state.selected_index)
            };
            return Some(TuiMouseEventResult {
                handled: Some(true),
                render: Some(selected_index != previous_index),
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

        // 条目区从搜索框 + 空行之后开始；没有搜索时 y 直接对应条目行。
        let row_offset = if search_enabled { 2 } else { 0 };
        let (start_index, end_index) = {
            let state = self.state.borrow();
            get_visible_range(state.selected_index, self.max_visible, display_len)
        };
        // 落在可见窗口之外（包括滚动后看不见的行）的坐标不处理。
        let item_index = start_index as isize + event.y - row_offset;
        if item_index < start_index as isize || item_index >= end_index as isize {
            return None;
        }
        if event.event_type == TuiMouseEventType::Press {
            self.mouse_pressed_index = Some(item_index);
            self.state.borrow_mut().selected_index = item_index;
            return Some(TuiMouseEventResult {
                handled: Some(true),
                focus: Some(true),
                ..Default::default()
            });
        }
        if event.event_type == TuiMouseEventType::Click {
            // 按下与抬起可能落在不同行：优先用按下时记录的行（TS `mousePressedIndex ?? itemIndex`）。
            let selected_index = self.mouse_pressed_index.unwrap_or(item_index);
            self.mouse_pressed_index = None;
            self.state.borrow_mut().selected_index = selected_index;
            SettingsListState::activate_item(&self.state);
            return Some(TuiMouseEventResult {
                handled: Some(true),
                ..Default::default()
            });
        }
        None
    }

    fn handle_input(&mut self, data: &str) {
        // 子菜单激活时把所有输入转交给它（它的 Escape 回调会通过 done() 关闭子菜单）。
        // `done()` 在子菜单的 `handle_input` 内部同步执行；本地 `Rc` 让组件活到调用返回。
        let submenu = self
            .state
            .borrow()
            .submenu_component
            .as_ref()
            .map(Rc::clone);
        if let Some(submenu) = submenu {
            submenu.borrow_mut().handle_input(data);
            return;
        }

        // 主列表输入。
        let kb = get_keybindings();
        let search_enabled = self.search_enabled();
        let display_len = self.state.borrow().get_display_items().len();
        // 空格算“切换取值”快捷键的条件：搜索关闭，或搜索框当前为空（TS 的
        // `!searchEnabled || searchInput?.getValue().length === 0`）。
        let search_empty = self
            .search_input
            .as_ref()
            .is_none_or(|search_input| search_input.get_value().is_empty());
        if kb.matches(data, "tui.select.up") {
            if display_len == 0 {
                return;
            }
            let mut state = self.state.borrow_mut();
            state.selected_index = if state.selected_index == 0 {
                display_len as isize - 1
            } else {
                state.selected_index - 1
            };
        } else if kb.matches(data, "tui.select.down") {
            if display_len == 0 {
                return;
            }
            let mut state = self.state.borrow_mut();
            state.selected_index = if state.selected_index == display_len as isize - 1 {
                0
            } else {
                state.selected_index + 1
            };
        } else if kb.matches(data, "tui.select.confirm")
            || (data == " " && (!search_enabled || search_empty))
        {
            SettingsListState::activate_item(&self.state);
        } else if kb.matches(data, "tui.select.cancel") {
            (self.on_cancel)();
        } else if search_enabled && let Some(search_input) = &mut self.search_input {
            // 其余按键全部交给搜索框，并用新查询重算过滤结果（选择重置到第一项）。
            search_input.handle_input(data);
            let query = search_input.get_value().to_string();
            self.state.borrow_mut().apply_filter(&query);
        }
    }

    /// 只把失效通知转发给子菜单；主列表没有跨帧缓存，每次 `render` 都重新计算。
    fn invalidate(&mut self) {
        let submenu = self
            .state
            .borrow()
            .submenu_component
            .as_ref()
            .map(Rc::clone);
        if let Some(submenu) = submenu {
            submenu.borrow_mut().invalidate();
        }
    }
}
