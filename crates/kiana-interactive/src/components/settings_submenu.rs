//! components/settings-submenu.ts。
//!
//! 两类子菜单：单步的 [`SelectSubmenu`]（可选模糊过滤）与多步的 [`SteppedSubmenu`]。
//!
//! ## 与 TS 的表示差异
//!
//! * TS 的回调是普通闭包；Rust 的组件是 `'static`，因此回调用 [`Rc`] 共享。TS 的
//!   `Record<string, string>` 上下文在 JS 里保持插入序（键都是普通字符串），Rust 用
//!   [`IndexMap`] 复刻；`delete context[key]` 对应 `shift_remove`。
//! * `SelectSubmenu` 在 TS 里 `extends Container`，过滤后通过
//!   `this.children[listChildIndex] = newList` 原地替换列表。Rust 侧改为**顺序固定的自绘
//!   布局**（字段顺序与 TS 的 addChild 顺序一致），过滤时直接替换 `select_list` 字段；
//!   [`Component::for_each_container_child`] 仍按 TS 的子组件顺序暴露 title/description/
//!   Input/SelectList/hint，便于焦点遍历找到搜索框。
//! * `SteppedSubmenu` 的当前步在 TS 里是可变的 `activeComponent`；Rust 用
//!   `Rc<RefCell<...>>` 共享，使得步进回调能在组件自己的 `handleInput` 调用栈里替换下一步，
//!   与 TS 的同步语义一致。
//! * 步骤上下文用 [`IndexMap`]（插入序）而不是 `spec/ca-interactive-mode.md` §5.3 草案里的
//!   `BTreeMap`；回调用 `Rc<dyn Fn>` 而不是草案里的 `Arc<dyn Fn>`——组件本身不是 `Send`
//!   （与 kiana-tui 的组件模型一致），`Arc` 会迫使闭包捕获 `Arc<Mutex<…>>`。
//!
//! ## 调用方与用途
//!
//! 本文件只提供通用构件，不含任何具体设置项；两个类型都由
//! `crate::components::settings_selector` 使用：
//!
//! * [`SelectSubmenu`] 由主题面板使用：单模式菜单整体就是一个 `SelectSubmenu`
//!   （`theme_show_single_menu`），自动模式下 Light/Dark 两个条目各自也是
//!   （`theme_create_select`）；
//! * [`SteppedSubmenu`] 用于需要多步才能确定一个值的设置项，例如 `model-thinking` 的
//!   “先选模型、再选思考等级”两步流程。
//!
//! 两者都只通过 `Component` 的 `render` / `handle_input` / `handle_mouse` 与宿主交互，
//! 不含定时器、线程或 IO。
//!
//! ## SteppedSubmenu 的状态机
//!
//! 状态集合很小：`子菜单` 始终只持有**当前一步**的 [`SelectSubmenu`]（`state.active`），
//! 外加共享的 `context: IndexMap<String, String>`。转换全部由当前步的回调触发：
//!
//! 1. **选中**（Enter）：把 `step.key → value` 写入 `context`；若不是最后一步就重建下一步；
//!    若是最后一步则先 `on_complete(context 的快照)`，然后
//!    * `looping == true`：清空 `context` 并回到第 0 步；
//!    * `looping == false`：**再调用 `on_cancel`**。这是 TS 的实际行为（完成即关闭面板，
//!      关闭路径统一走 `onCancel`），由测试
//!      `stepped_submenu_completes_and_cancels_without_loop` 锁定，不是笔误。
//! 2. **取消**（Esc）：`step_index > 0` 时删除**当前步**的键并回到上一步（更早步骤的选择
//!    保留）；第 0 步则直接调用 `on_cancel`。
//!
//! 不变量：`context` 的键来自步骤的 `key` 字段，同名 `key` 会互相覆盖；`start_at_step`
//! 跳过的步骤必须由 `initial_context` 预填，否则它们对应的键在完成回调里缺失。
//! 越界的 `start_at_step`（或空 `steps` 且下标 0 也不存在）在 TS 里抛 TypeError，Rust 侧
//! 退化为一个空 [`Spacer`] 占位，不会 panic。
//!
//! ## 输入与鼠标
//!
//! * [`SelectSubmenu`] 有搜索框时，只有 `tui.select.{up,down,confirm,cancel}` 四类按键交给
//!   列表，其余按键写入搜索框并重新过滤；没有搜索框时全部按键直达列表。
//! * [`SelectSubmenu`] 的 `handle_mouse` 靠“按与实际渲染相同的顺序重渲染各子项求高度”来
//!   确定命中行，子项顺序必须与 `render` 一致，否则点击会落到错误的行。
//! * [`SteppedSubmenu`] 在 TS 里继承 `Container` 却从不 `addChild`，所以鼠标事件完全无效；
//!   Rust 侧直接返回 `None` 复刻这一行为（见该方法的说明）。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/settings-submenu.ts`。

use std::cell::RefCell;
use std::rc::Rc;

use indexmap::IndexMap;
use kiana_theme::get_select_list_theme;
use kiana_tui::{
    Component, Input, SelectItem, SelectList, SelectListLayoutOptions, Spacer, Text, fuzzy_filter,
    get_keybindings,
};

/// `SUBMENU_SELECT_LIST_LAYOUT`（模块私有常量）。
fn submenu_select_list_layout() -> SelectListLayoutOptions {
    SelectListLayoutOptions {
        min_primary_column_width: Some(12.0),
        max_primary_column_width: Some(32.0),
        truncate_primary: None,
    }
}

/// TS `theme.bold(theme.fg("accent", text))`（SelectSubmenu 标题的顺序）。
fn bold_accent(text: &str) -> String {
    let accent = kiana_theme::THEME
        .fg(kiana_theme::ThemeColor::Accent, text)
        .expect("theme must be initialized before rendering (TS throws here)");
    kiana_theme::THEME
        .bold(&accent)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// TS `theme.fg("muted", text)`。
fn muted(text: &str) -> String {
    kiana_theme::THEME
        .fg(kiana_theme::ThemeColor::Muted, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// TS `theme.fg("dim", text)`。
fn dim(text: &str) -> String {
    kiana_theme::THEME
        .fg(kiana_theme::ThemeColor::Dim, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// `SelectSubmenu` 的回调（TS `(value: string) => void`）。
pub type SelectValueCallback = Rc<dyn Fn(&str)>;
/// `SelectSubmenu` 的取消回调（TS `() => void`）。
pub type VoidCallback = Rc<dyn Fn()>;
/// 步骤标题/描述的动态求值闭包。
///
/// 参数是**当前**的步骤上下文；返回值直接作为标题/描述的原始文本（未着色，着色由
/// `SelectSubmenu` 加）。闭包在每次进入该步时都会被调用，不缓存结果。
pub type StepTextCallback = Rc<dyn Fn(&IndexMap<String, String>) -> String>;
/// 步骤选项生成闭包。
///
/// 每次进入该步都重新调用，因此后续步骤可以根据先前步骤的选择动态生成选项。
pub type StepOptionsCallback = Rc<dyn Fn(&IndexMap<String, String>) -> Vec<SelectItem>>;
/// 步骤预选闭包。
///
/// 返回 `Some(value)` 时按 `SelectItem.value` 匹配预选；返回 `None` 或匹配不到时保持
/// 列表默认的第 0 项。
pub type StepPreselectCallback = Rc<dyn Fn(&IndexMap<String, String>) -> Option<String>>;
/// 多步完成回调（TS `onComplete(context)`）。
///
/// 参数是完成后上下文的**快照**（克隆），此后 `looping` 模式会清空内部上下文，因此
/// 回调需要保留的值应自行克隆。
pub type StepCompleteCallback = Rc<dyn Fn(&IndexMap<String, String>)>;

/// TS `SelectSubmenuOptions`。
///
/// 两个字段都是 `Option`，`None` 表示“用默认”：`searchable` 缺省为 `false`，`layout`
/// 缺省为模块内的 `SUBMENU_SELECT_LIST_LAYOUT`（主列 12..=32 列）。
#[derive(Debug, Clone, Default)]
pub struct SelectSubmenuOptions {
    /// 启用输入即过滤的模糊搜索；`None` 视为 `false`。
    pub searchable: Option<bool>,
    /// 覆盖列宽布局；`None` 用 `SUBMENU_SELECT_LIST_LAYOUT`。
    pub layout: Option<SelectListLayoutOptions>,
}

/// 单步子菜单（TS `class SelectSubmenu extends Container`）。
///
/// 渲染顺序固定为 title →（Spacer + description）→（Spacer + 搜索框）→ Spacer →
/// 选择列表 → Spacer → 操作提示，与该结构体的字段顺序一致；`render`、`handle_mouse` 与
/// `for_each_container_child` 三处都必须遵守同一顺序，改一处就要同步另外两处。
pub struct SelectSubmenu {
    /// 顶部标题（已着色加粗）。
    title: Text,
    /// 可选说明行；空字符串表示不渲染。
    description: Option<Text>,
    /// `searchable` 为真时存在；输入即过滤。
    search_input: Option<Input>,
    /// 与 TS 的 `this.selectList` 一样可被 `applyFilter` 原地替换；用 `Rc` 让搜索框的
    /// `onSubmit` 始终指向“当前”列表。
    select_list: Rc<RefCell<SelectList>>,
    /// 底部固定提示（是否含 “Type to filter” 取决于 `searchable`）。
    hint: Text,
    /// 未过滤的完整选项集；`apply_filter` 每次都从这里重新过滤。
    all_options: Vec<SelectItem>,
    /// 构建每个 `SelectList` 时复用的列宽布局。
    list_layout: SelectListLayoutOptions,
    /// 列表确认回调（参数是 `SelectItem.value`）。
    on_select_cb: SelectValueCallback,
    /// 列表取消回调（Esc）。
    on_cancel_cb: VoidCallback,
    /// 高亮变化回调；过滤重建列表后也会重新挂上。
    on_selection_change_cb: Option<SelectValueCallback>,
}

impl SelectSubmenu {
    /// TS 8 参数构造（`onSelectionChange` 与 `submenuOptions` 可选）。
    ///
    /// 这里固定传 `None` 与默认选项：没有高亮变化回调、没有搜索框，列宽取本模块的
    /// `SUBMENU_SELECT_LIST_LAYOUT`。需要这些能力时直接用
    /// [`SelectSubmenu::with_callbacks`]。本仓库内没有调用点，保留它是为了对应 TS 的重载。
    pub fn new(
        title: &str,
        description: &str,
        options: Vec<SelectItem>,
        current_value: &str,
        on_select: SelectValueCallback,
        on_cancel: VoidCallback,
    ) -> Self {
        SelectSubmenu::with_callbacks(
            title,
            description,
            options,
            current_value,
            on_select,
            on_cancel,
            None,
            SelectSubmenuOptions::default(),
        )
    }

    /// 完整构造（对应 TS 的 8 个位置参数）。
    ///
    /// `submenu_options` 为 `None` 时：布局取 `SUBMENU_SELECT_LIST_LAYOUT`（主列 12..=32 列），
    /// **不启用**搜索框。`current_value` 按 `SelectItem.value` 匹配，匹配不到就保持列表默认
    /// 的第 0 项。
    ///
    /// `options` 会被复制两份：一份交给首个 `SelectList`，一份存进 `all_options` 供之后
    /// 的过滤重建使用。
    // TS 构造函数就是 8 个位置参数；保持同一顺序便于逐行对照。
    #[allow(clippy::too_many_arguments)]
    pub fn with_callbacks(
        title: &str,
        description: &str,
        options: Vec<SelectItem>,
        current_value: &str,
        on_select: SelectValueCallback,
        on_cancel: VoidCallback,
        on_selection_change: Option<SelectValueCallback>,
        submenu_options: SelectSubmenuOptions,
    ) -> Self {
        let list_layout = submenu_options
            .layout
            .clone()
            .unwrap_or_else(submenu_select_list_layout);
        let searchable = submenu_options.searchable.unwrap_or(false);
        let all_options = options.clone();

        let select_list = Rc::new(RefCell::new(build_select_list(
            options,
            current_value,
            &list_layout,
            &on_select,
            &on_cancel,
            on_selection_change.as_ref(),
        )));

        let mut search_input = if searchable {
            Some(Input::default())
        } else {
            None
        };
        if let Some(input) = search_input.as_mut() {
            // TS `searchInput.onSubmit = () => this.selectList.handleInput("\r")`。
            // 正常键位下 Enter 属于导航键、不会进入搜索框；自定义键位时才走这条路径。
            let list = Rc::clone(&select_list);
            input.on_submit = Some(Box::new(move |_value: &str| {
                list.borrow_mut().handle_input("\r");
            }));
        }

        let hint = if searchable {
            "  Type to filter \u{b7} Enter to select \u{b7} Esc to go back"
        } else {
            "  Enter to select \u{b7} Esc to go back"
        };

        SelectSubmenu {
            title: Text::with_padding(bold_accent(title), 0, 0),
            description: if description.is_empty() {
                None
            } else {
                Some(Text::with_padding(muted(description), 0, 0))
            },
            search_input,
            select_list,
            hint: Text::with_padding(dim(hint), 0, 0),
            all_options,
            list_layout,
            on_select_cb: on_select,
            on_cancel_cb: on_cancel,
            on_selection_change_cb: on_selection_change,
        }
    }

    /// `buildSelectList(options, preselect)`：用本组件保存的布局与三个回调重建列表。
    ///
    /// 过滤重建（`apply_filter`）与首次构造走的是同一个 [`build_select_list`] 函数，
    /// 区别只在参数来源：这里从 `&self` 读取 `list_layout` 与三个回调。
    fn build_select_list_for(&self, options: Vec<SelectItem>, preselect: &str) -> SelectList {
        build_select_list(
            options,
            preselect,
            &self.list_layout,
            &self.on_select_cb,
            &self.on_cancel_cb,
            self.on_selection_change_cb.as_ref(),
        )
    }

    /// `applyFilter(query)`：模糊过滤并**原地替换**列表（preselect 传空串 → 回到第一项）。
    ///
    /// 名字沿用 TS；实现与 `SelectList::set_filter` 无关——这里重建整个列表对象，所以新列表
    /// 的选中项按 `build_select_list` 的规则重新确定（空 preselect → 第 0 行），旧列表的
    /// 选中位置不会保留，`on_selection_change` 也不会因过滤本身被调用。
    ///
    /// 只在 `searchable` 为真时被 `handle_input` 调用。每次输入都从 `all_options` 重新
    /// 过滤（不是在上一轮结果上继续收窄），因此删除字符能恢复更多选项；过滤键是
    /// `"{label} {description}"` 的拼接串。
    fn apply_filter(&mut self, query: &str) {
        let filtered = if query.is_empty() {
            self.all_options.clone()
        } else {
            fuzzy_filter(self.all_options.clone(), query, |item| {
                format!(
                    "{} {}",
                    item.label,
                    item.description.as_deref().unwrap_or("")
                )
            })
        };
        let new_list = self.build_select_list_for(filtered, "");
        *self.select_list.borrow_mut() = new_list;
    }

    /// 当前选中下标（TS `selectList.selectedIndex`，供测试/宿主观察）。
    pub fn selected_index(&self) -> isize {
        self.select_list.borrow().selected_index()
    }

    /// 搜索框当前值（没有搜索框时为空串）。
    ///
    /// 返回值是克隆的 `String`，不借此持有搜索框的借用。
    pub fn search_value(&self) -> String {
        self.search_input
            .as_ref()
            .map(|input| input.get_value().to_string())
            .unwrap_or_default()
    }
}

/// 共享回调的 `buildSelectList` 实现。
///
/// `max_visible` 是 `min(options.len(), 10)`：`SelectList` 只渲染以选中项为中心的窗口，
/// 传 10 表示最多显示 10 行（选项更少时自然只显示全部）。`preselect` 按
/// `SelectItem.value` 查找（不是 label），找不到就不改变默认的选中项。三个回调都被复制进
/// 列表，因此重建列表后行为一致。
fn build_select_list(
    options: Vec<SelectItem>,
    preselect: &str,
    layout: &SelectListLayoutOptions,
    on_select: &SelectValueCallback,
    on_cancel: &VoidCallback,
    on_selection_change: Option<&SelectValueCallback>,
) -> SelectList {
    let mut list = SelectList::new(
        options.clone(),
        options.len().min(10),
        get_select_list_theme(),
        layout.clone(),
    );
    if let Some(index) = options.iter().position(|option| option.value == preselect) {
        list.set_selected_index(index as isize);
    }
    let select_cb = Rc::clone(on_select);
    list.on_select = Some(Box::new(move |item: &SelectItem| select_cb(&item.value)));
    let cancel_cb = Rc::clone(on_cancel);
    list.on_cancel = Some(Box::new(move || cancel_cb()));
    if let Some(callback) = on_selection_change {
        let callback = Rc::clone(callback);
        list.on_selection_change = Some(Box::new(move |item: &SelectItem| callback(&item.value)));
    }
    list
}

impl Component for SelectSubmenu {
    /// 按固定顺序拼装输出（见类型文档）。`Spacer` 只在对应可选项存在时插入，所以无
    /// description、无搜索框时不会多出空行。
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        lines.extend(self.title.render(width));
        if let Some(description) = &mut self.description {
            lines.extend(Spacer::new().render(width));
            lines.extend(description.render(width));
        }
        if let Some(input) = &mut self.search_input {
            lines.extend(Spacer::new().render(width));
            lines.extend(input.render(width));
        }
        lines.extend(Spacer::new().render(width));
        lines.extend(self.select_list.borrow_mut().render(width));
        lines.extend(Spacer::new().render(width));
        lines.extend(self.hint.render(width));
        lines
    }

    /// 有搜索框时按“导航键 vs 其它键”分流：导航键直接驱动列表，其它键进入搜索框并重新
    /// 过滤。注意过滤发生在写入字符之后，所以每输入一个字符列表就会重建一次。
    fn handle_input(&mut self, data: &str) {
        if self.search_input.is_some() {
            let kb = get_keybindings();
            let is_nav = kb.matches(data, "tui.select.up")
                || kb.matches(data, "tui.select.down")
                || kb.matches(data, "tui.select.confirm")
                || kb.matches(data, "tui.select.cancel");
            if is_nav {
                self.select_list.borrow_mut().handle_input(data);
            } else {
                let query = {
                    let input = self
                        .search_input
                        .as_mut()
                        .expect("search_input checked above");
                    input.handle_input(data);
                    input.get_value().to_string()
                };
                self.apply_filter(&query);
            }
        } else {
            self.select_list.borrow_mut().handle_input(data);
        }
    }

    /// 按 `render` 的顺序现场重渲染各子项、累加高度，把事件坐标换算到目标子项的局部行号
    /// 后转发。这里不复用上一帧的布局：它始终用 `event.width` 重新求高度，因此只要调用方
    /// 传入的宽度与最近一次渲染一致，命中就是准确的；子项顺序和 `render` 不同步时点击会
    /// 落到相邻行。未命中任何子项返回 `None`，由外层继续处理（例如冒泡给别的组件）。
    fn handle_mouse(
        &mut self,
        event: &kiana_tui::TuiMouseEvent,
    ) -> Option<kiana_tui::TuiMouseEventResult> {
        let width = event.width;
        let mut offset = self.title.render(width).len() as isize;
        if let Some(description) = &mut self.description {
            offset += Spacer::new().render(width).len() as isize;
            offset += description.render(width).len() as isize;
        }
        if let Some(input) = &mut self.search_input {
            offset += Spacer::new().render(width).len() as isize;
            let height = input.render(width).len() as isize;
            if event.y >= offset && event.y < offset + height {
                let mut local = event.clone();
                local.y -= offset;
                local.height = height.max(0) as usize;
                return input.handle_mouse(&local);
            }
            offset += height;
        }
        offset += Spacer::new().render(width).len() as isize;
        {
            let mut select_list = self.select_list.borrow_mut();
            let height = select_list.render(width).len() as isize;
            if event.y >= offset && event.y < offset + height {
                let mut local = event.clone();
                local.y -= offset;
                local.height = height.max(0) as usize;
                return select_list.handle_mouse(&local);
            }
        }
        None
    }

    /// 让所有子项丢弃渲染缓存；`Rc<RefCell<SelectList>>` 里的列表同样被失效。
    fn invalidate(&mut self) {
        self.title.invalidate();
        if let Some(description) = &mut self.description {
            description.invalidate();
        }
        if let Some(input) = &mut self.search_input {
            input.invalidate();
        }
        self.select_list.borrow_mut().invalidate();
        self.hint.invalidate();
    }

    /// 按渲染顺序暴露子项，供宿主做焦点遍历（例如把焦点落到搜索框）。中途某次 `f`
    /// 返回 `false` 就停止遍历。
    fn for_each_container_child(&self, f: &mut dyn FnMut(&dyn Component) -> bool) {
        if !f(&self.title) {
            return;
        }
        if let Some(description) = &self.description
            && !f(description)
        {
            return;
        }
        if let Some(input) = &self.search_input
            && !f(input)
        {
            return;
        }
        {
            let select_list = self.select_list.borrow();
            if !f(&*select_list) {
                return;
            }
        }
        let _ = f(&self.hint);
    }

    /// 与 [`Component::for_each_container_child`] 相同的顺序，交给需要可变访问的调用方。
    fn for_each_container_child_mut(
        &mut self,
        f: &mut dyn FnMut(&mut (dyn Component + 'static)) -> bool,
    ) {
        if !f(&mut self.title) {
            return;
        }
        if let Some(description) = &mut self.description
            && !f(description)
        {
            return;
        }
        if let Some(input) = &mut self.search_input
            && !f(input)
        {
            return;
        }
        {
            let mut select_list = self.select_list.borrow_mut();
            if !f(&mut *select_list) {
                return;
            }
        }
        let _ = f(&mut self.hint);
    }
}

// ============================================================================
// SteppedSubmenu — reusable multi-step selector
// ============================================================================

/// 步骤标题/描述的来源（TS `string | ((context) => string)`）。
#[derive(Clone)]
pub enum StepText {
    /// 固定字符串。
    Fixed(String),
    /// 依上下文动态生成。
    Dynamic(StepTextCallback),
}

impl StepText {
    /// 按当前上下文求值：`Fixed` 返回克隆的字符串，`Dynamic` 调用闭包。
    pub fn resolve(&self, context: &IndexMap<String, String>) -> String {
        match self {
            StepText::Fixed(text) => text.clone(),
            StepText::Dynamic(func) => func(context),
        }
    }
}

impl From<&str> for StepText {
    fn from(value: &str) -> Self {
        StepText::Fixed(value.to_string())
    }
}

impl From<String> for StepText {
    fn from(value: String) -> Self {
        StepText::Fixed(value)
    }
}

impl std::fmt::Debug for StepText {
    /// 手工实现的 `Debug`：闭包无法格式化，`Dynamic` 只打印成 `Dynamic(<fn>)`，避免
    /// derive 无法用于 `Rc<dyn Fn>` 的问题。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepText::Fixed(text) => formatter.debug_tuple("Fixed").field(text).finish(),
            StepText::Dynamic(_) => formatter.write_str("Dynamic(<fn>)"),
        }
    }
}

/// TS `SteppedSubmenuStep`。
#[derive(Clone)]
pub struct SteppedSubmenuStep {
    /// 唯一键；选中值写入上下文的这个键。
    pub key: String,
    /// 步骤顶部标题（固定文本或按上下文求值）。
    pub title: StepText,
    /// 标题下方的描述。步骤数大于 1 时，`build_step` 会在其前面拼上
    /// `Step {n}/{total} · ` 前缀。
    pub description: StepText,
    /// 依上下文生成选项列表（每次展示该步都会重新调用）。
    pub options: StepOptionsCallback,
    /// 进入该步时的预选值。
    pub preselect: Option<StepPreselectCallback>,
    /// 该步是否启用模糊搜索。
    pub searchable: Option<bool>,
    /// 该步的列宽布局。
    pub layout: Option<SelectListLayoutOptions>,
}

impl SteppedSubmenuStep {
    /// 构造一个基础步骤（无预选/搜索/布局覆盖）。
    pub fn new(
        key: impl Into<String>,
        title: impl Into<StepText>,
        description: impl Into<StepText>,
        options: StepOptionsCallback,
    ) -> Self {
        SteppedSubmenuStep {
            key: key.into(),
            title: title.into(),
            description: description.into(),
            options,
            preselect: None,
            searchable: None,
            layout: None,
        }
    }

    /// 设置 `preselect`。
    pub fn with_preselect(mut self, preselect: StepPreselectCallback) -> Self {
        self.preselect = Some(preselect);
        self
    }

    /// 设置 `searchable`。
    pub fn with_searchable(mut self, searchable: bool) -> Self {
        self.searchable = Some(searchable);
        self
    }

    /// 设置 `layout`。
    pub fn with_layout(mut self, layout: SelectListLayoutOptions) -> Self {
        self.layout = Some(layout);
        self
    }
}

/// TS `SteppedSubmenuOptions`（模块私有 interface）。
#[derive(Debug, Clone, Default)]
pub struct SteppedSubmenuOptions {
    /// 起始步（0 基）；跳过前面步骤时需要 `initial_context`。
    pub start_at_step: Option<usize>,
    /// 预填被跳过步骤的选择值。
    pub initial_context: Option<IndexMap<String, String>>,
    /// 完成最后一步后回到第 0 步而不是关闭（TS 的 `loop`）。
    ///
    /// 为真时还会在回到第 0 步前清空上下文；为假时完成最后一步会**调用 `on_cancel`**
    /// （关闭面板的统一路径），见模块文档的状态机说明。
    pub looping: bool,
}

/// `SteppedSubmenu` 的全部可变状态，与当前步、下一步、完成/取消回调共享同一个 `Rc`。
///
/// 共享的原因：`SelectSubmenu` 的回调是 `'static` 闭包，必须在组件自己的 `handle_input`
/// 调用栈里替换 `active`（换成下一步或上一步），而 `Rc<RefCell<…>>` 是这条同步路径的
/// 载体。`active` 额外再包一层 `Rc<RefCell<Box<dyn Component>>>`，让 `render`/`handle_input`
/// 可以先克隆句柄、释放外层借用，再借用当前步，避免同一 `RefCell` 的重入借用 panic。
struct SteppedSubmenuState {
    /// 步骤定义；下标即 `step_index`。
    steps: Vec<SteppedSubmenuStep>,
    /// 最后一步完成后调用，参数是上下文快照。
    on_complete: StepCompleteCallback,
    /// 取消/关闭回调（Loop 模式下完成不会调用它）。
    on_cancel: Rc<dyn Fn()>,
    /// `start_at_step` / `initial_context` / `looping`。
    opts: SteppedSubmenuOptions,
    /// 当前步的组件；构造期先放空 `Spacer`，随后立即替换为 `build_step(start)` 的结果。
    active: Rc<RefCell<Box<dyn Component>>>,
    /// 已选值，键为步骤的 `key`。
    context: IndexMap<String, String>,
}

/// 多步子菜单（TS `class SteppedSubmenu extends Container`）。
///
/// 只持有共享状态；对外表现是“当前步”的渲染与输入，步骤切换完全由当前步列表的回调驱动。
pub struct SteppedSubmenu {
    state: Rc<RefCell<SteppedSubmenuState>>,
}

impl SteppedSubmenu {
    /// `constructor(steps, onComplete, onCancel, opts = {})`。
    ///
    /// `start_at_step` 缺省为 0；`initial_context` 复制进状态（调用方之后修改传入的 map
    /// 不会影响组件）。构造期先以空 `Spacer` 占位 `active`，再调用 `build_step`：后者需要
    /// 在 `state.borrow()` 下读取上下文，返回的新组件不能同时写回同一个借用。
    pub fn new(
        steps: Vec<SteppedSubmenuStep>,
        on_complete: StepCompleteCallback,
        on_cancel: Rc<dyn Fn()>,
        opts: SteppedSubmenuOptions,
    ) -> Self {
        let start = opts.start_at_step.unwrap_or(0);
        let context = opts.initial_context.clone().unwrap_or_default();
        let state = Rc::new(RefCell::new(SteppedSubmenuState {
            steps,
            on_complete,
            on_cancel,
            opts,
            active: Rc::new(RefCell::new(Box::new(Spacer::new()))),
            context,
        }));
        let first = {
            let state_ref = state.borrow();
            build_step(&state, &state_ref, start)
        };
        state.borrow_mut().active = Rc::new(RefCell::new(first));
        SteppedSubmenu { state }
    }

    /// 当前步骤上下文（只读）。
    ///
    /// 返回克隆：调用方无法借此观察后续步骤的变化，也不能修改内部状态。`looping` 模式
    /// 完成一轮后会清空上下文，因此这里可能返回空 map。
    pub fn context(&self) -> IndexMap<String, String> {
        self.state.borrow().context.clone()
    }
}

/// TS `buildStep(stepIndex)`。
///
/// 每次进入某一步都会重新调用：`title` / `description` / `options` / `preselect` 四个
/// 闭包都拿到**当前**上下文求值，因此后续步骤能看到前面步骤的选择。
///
/// 步骤数大于 1 时把 `Step {n}/{total} · ` 前缀拼进 description（只是给描述加进度提示，
/// 标题行不受影响）。`SelectSubmenuOptions` 只在 `searchable == Some(true)` 或给了 `layout` 时构造，
/// 否则用默认值——这样“显式 searchable: false + 无 layout”与“完全没写”走同一条路径。
///
/// 返回 `Box<dyn Component>`：越界下标（含空 `steps`）返回空 [`Spacer`]，替代 TS 的
/// TypeError。
fn build_step(
    state: &Rc<RefCell<SteppedSubmenuState>>,
    state_ref: &SteppedSubmenuState,
    step_index: usize,
) -> Box<dyn Component> {
    let Some(step) = state_ref.steps.get(step_index) else {
        // TS 在空 steps 上构造会抛 TypeError；Rust 侧用一个空占位组件代替（正常调用不会出现）。
        return Box::new(Spacer::new());
    };
    let total = state_ref.steps.len();
    let step_label = if total > 1 {
        format!("Step {}/{} \u{b7} ", step_index + 1, total)
    } else {
        String::new()
    };
    let title = step.title.resolve(&state_ref.context);
    let description = step.description.resolve(&state_ref.context);
    let items = (step.options)(&state_ref.context);
    let preselect = step
        .preselect
        .as_ref()
        .and_then(|preselect| preselect(&state_ref.context))
        .unwrap_or_default();

    let select_key = step.key.clone();
    let cancel_key = step.key.clone();
    let select_state = Rc::clone(state);
    let cancel_state = Rc::clone(state);
    let select = Rc::new(move |value: &str| {
        step_selected(&select_state, step_index, &select_key, value);
    });
    let cancel = Rc::new(move || {
        step_cancelled(&cancel_state, step_index, &cancel_key);
    });

    let submenu_options = if step.searchable == Some(true) || step.layout.is_some() {
        SelectSubmenuOptions {
            searchable: step.searchable,
            layout: step.layout.clone(),
        }
    } else {
        SelectSubmenuOptions::default()
    };
    Box::new(SelectSubmenu::with_callbacks(
        &title,
        &format!("{step_label}{description}"),
        items,
        &preselect,
        select,
        cancel,
        None,
        submenu_options,
    ))
}

/// 步骤选中后的状态迁移（对应 TS `buildStep` 的 `onSelect`）。
///
/// 三个阶段：
/// 1. 写入 `context[step.key]`；
/// 2. 非最后一步 → 在持有 `state` 的可变借用时构建并安装下一步。`build_step` 接收
///    `&state_ref` 只是为了读上下文，不能反过来再 `borrow()` 同一个 `RefCell`。
/// 3. 最后一步 → 先克隆上下文快照与两个回调、`drop` 借用，再调用 `on_complete`。回调在
///    借用释放之后执行，因此回调内部可以再次借用组件状态而不会触发 `RefCell` 的双重
///    可变借用 panic。`looping` 为真时清空上下文回到第 0 步；否则重新借用并调用
///    `on_cancel`（完成后面板走关闭路径）。
fn step_selected(
    state: &Rc<RefCell<SteppedSubmenuState>>,
    step_index: usize,
    key: &str,
    value: &str,
) {
    let mut state_ref = state.borrow_mut();
    state_ref.context.insert(key.to_string(), value.to_string());
    let total = state_ref.steps.len();

    if step_index < total.saturating_sub(1) {
        let next = build_step(state, &state_ref, step_index + 1);
        state_ref.active = Rc::new(RefCell::new(next));
        return;
    }

    let snapshot = state_ref.context.clone();
    let on_complete = Rc::clone(&state_ref.on_complete);
    let on_cancel = Rc::clone(&state_ref.on_cancel);
    let looping = state_ref.opts.looping;
    drop(state_ref);

    on_complete(&snapshot);

    let mut state_ref = state.borrow_mut();
    if looping {
        state_ref.context.clear();
        let next = build_step(state, &state_ref, 0);
        state_ref.active = Rc::new(RefCell::new(next));
    } else {
        drop(state_ref);
        on_cancel();
    }
}

/// Esc：回到上一步；第 0 步则取消（对应 TS `buildStep` 的 `onCancel`）。
///
/// 回退时删除的是**当前步的键**（`shift_remove` 保持其余键的插入序），更早步骤的选择
/// 保留在上下文里；再次前进时这些步骤会重新求值并覆盖同名的键。
fn step_cancelled(state: &Rc<RefCell<SteppedSubmenuState>>, step_index: usize, key: &str) {
    if step_index > 0 {
        let mut state_ref = state.borrow_mut();
        state_ref.context.shift_remove(key);
        let previous = build_step(state, &state_ref, step_index - 1);
        state_ref.active = Rc::new(RefCell::new(previous));
    } else {
        let on_cancel = Rc::clone(&state.borrow().on_cancel);
        on_cancel();
    }
}

impl Component for SteppedSubmenu {
    /// 只渲染当前步。
    ///
    /// 先把 `active` 句柄克隆出来、结束对外层 `state` 的借用，再去借用当前步组件；
    /// 与 `handle_input` 同一写法，使渲染期间不会一直占用外层状态的借用。
    fn render(&mut self, width: usize) -> Vec<String> {
        let active = Rc::clone(&self.state.borrow().active);
        active.borrow_mut().render(width)
    }

    /// 全部按键交给当前步；步骤切换发生在当前步 `SelectSubmenu` 的回调里，返回时
    /// `active` 可能已经被替换。
    fn handle_input(&mut self, data: &str) {
        let active = Rc::clone(&self.state.borrow().active);
        active.borrow_mut().handle_input(data);
    }

    /// TS `SteppedSubmenu extends Container` 但**从不** `addChild(activeComponent)`：
    /// `Container.handleMouse` 遍历空 `children`，所以鼠标事件在 SteppedSubmenu 内
    /// 完全无效（点击/滚轮不会改变当前步骤的选择）。
    fn handle_mouse(
        &mut self,
        _event: &kiana_tui::TuiMouseEvent,
    ) -> Option<kiana_tui::TuiMouseEventResult> {
        None
    }

    /// 只让当前步失效；非当前步的组件在切换时已经被替换丢弃，无需处理。
    fn invalidate(&mut self) {
        let active = Rc::clone(&self.state.borrow().active);
        active.borrow_mut().invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| kiana_theme::init_theme(Some("dark"), false));
    }

    fn item(value: &str, label: &str) -> SelectItem {
        SelectItem::with_label(value, label)
    }

    #[test]
    fn select_submenu_preselects_but_filter_resets_to_first() {
        init();
        let selected = Rc::new(RefCell::new(Vec::<String>::new()));
        let sink = Rc::clone(&selected);
        let submenu = SelectSubmenu::with_callbacks(
            "Pick",
            "Description",
            vec![item("a", "Alpha"), item("b", "Beta")],
            "b",
            Rc::new(move |value| sink.borrow_mut().push(value.to_string())),
            Rc::new(|| {}),
            None,
            SelectSubmenuOptions {
                searchable: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(submenu.selected_index(), 1);
        assert_eq!(submenu.search_value(), "");
    }

    #[test]
    fn select_submenu_searchable_filter_resets_selection_and_reports_value() {
        init();
        let selected = Rc::new(RefCell::new(Vec::<String>::new()));
        let sink = Rc::clone(&selected);
        let mut submenu = SelectSubmenu::with_callbacks(
            "Pick",
            "Description",
            vec![item("alpha", "Alpha"), item("beta", "Beta")],
            "beta",
            Rc::new(move |value| sink.borrow_mut().push(value.to_string())),
            Rc::new(|| {}),
            None,
            SelectSubmenuOptions {
                searchable: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(submenu.selected_index(), 1);
        submenu.handle_input("b");
        submenu.handle_input("e");
        // 过滤后 preselect 传空串 → 选中回到第一项（beta 是唯一命中项）。
        assert_eq!(submenu.selected_index(), 0);
        assert_eq!(submenu.search_value(), "be");
        submenu.handle_input("\r");
        assert_eq!(*selected.borrow(), vec!["beta".to_string()]);
    }

    #[test]
    fn stepped_submenu_prepends_step_label_to_description() {
        init();
        let steps = vec![
            SteppedSubmenuStep::new(
                "one",
                "First title",
                "First description",
                Rc::new(|_| vec![item("x", "X")]),
            ),
            SteppedSubmenuStep::new(
                "two",
                "Second title",
                "Second description",
                Rc::new(|_| vec![item("y", "Y")]),
            ),
        ];
        let mut submenu = SteppedSubmenu::new(
            steps,
            Rc::new(|_| {}),
            Rc::new(|| {}),
            SteppedSubmenuOptions::default(),
        );
        let rendered = submenu.render(80).join("\n");
        assert!(rendered.contains("First title"));
        assert!(rendered.contains("Step 1/2 \u{b7} First description"));
        submenu.handle_input("\r");
        let rendered = submenu.render(80).join("\n");
        assert!(rendered.contains("Second title"));
        assert!(rendered.contains("Step 2/2 \u{b7} Second description"));
    }

    #[test]
    fn stepped_submenu_completes_and_cancels_without_loop() {
        init();
        let completed = Rc::new(RefCell::new(Vec::<IndexMap<String, String>>::new()));
        let cancelled = Rc::new(RefCell::new(0usize));
        let completed_sink = Rc::clone(&completed);
        let cancelled_sink = Rc::clone(&cancelled);
        let steps = vec![SteppedSubmenuStep::new(
            "k",
            "T",
            "D",
            Rc::new(|_| vec![item("v", "V")]),
        )];
        let mut submenu = SteppedSubmenu::new(
            steps,
            Rc::new(move |context| completed_sink.borrow_mut().push(context.clone())),
            Rc::new(move || *cancelled_sink.borrow_mut() += 1),
            SteppedSubmenuOptions::default(),
        );
        submenu.handle_input("\r");
        assert_eq!(completed.borrow().len(), 1);
        assert_eq!(
            completed.borrow()[0].get("k").map(String::as_str),
            Some("v")
        );
        // 非 loop：完成最后一步后**还会**调用 onCancel（TS 的实际行为）。
        assert_eq!(*cancelled.borrow(), 1);
    }

    #[test]
    fn stepped_submenu_loop_clears_context_and_returns_to_first_step() {
        init();
        let completed = Rc::new(RefCell::new(Vec::<IndexMap<String, String>>::new()));
        let completed_sink = Rc::clone(&completed);
        let cancelled = Rc::new(RefCell::new(0usize));
        let cancelled_sink = Rc::clone(&cancelled);
        let steps = vec![SteppedSubmenuStep::new(
            "k",
            "T",
            "D",
            Rc::new(|_| vec![item("v", "V")]),
        )];
        let mut submenu = SteppedSubmenu::new(
            steps,
            Rc::new(move |context| completed_sink.borrow_mut().push(context.clone())),
            Rc::new(move || *cancelled_sink.borrow_mut() += 1),
            SteppedSubmenuOptions {
                looping: true,
                ..Default::default()
            },
        );
        submenu.handle_input("\r");
        assert_eq!(completed.borrow().len(), 1);
        assert_eq!(*cancelled.borrow(), 0);
        assert!(submenu.context().is_empty());
    }

    #[test]
    fn stepped_submenu_escape_goes_back_one_step() {
        init();
        let steps = vec![
            SteppedSubmenuStep::new("a", "A", "DA", Rc::new(|_| vec![item("a1", "A1")])),
            SteppedSubmenuStep::new("b", "B", "DB", Rc::new(|_| vec![item("b1", "B1")])),
        ];
        let mut submenu = SteppedSubmenu::new(
            steps,
            Rc::new(|_| {}),
            Rc::new(|| {}),
            SteppedSubmenuOptions::default(),
        );
        submenu.handle_input("\r");
        assert!(submenu.render(80).join("\n").contains("Step 2/2"));
        submenu.handle_input("\x1b");
        assert!(submenu.render(80).join("\n").contains("Step 1/2"));
        // Esc 只删除**当前步**的键：第 0 步已经写入的 "a" 保留。
        assert_eq!(submenu.context().get("a").map(String::as_str), Some("a1"));
        assert!(!submenu.context().contains_key("b"));
    }
}
