//! components/thinking-selector.ts。
//!
//! `/thinking` 选择器：搜索框 + `SelectList`，选中项带 `✓ ` 标记；`app.thinking.save`
//! 把当前项设为默认。
//!
//! # 调用方与数据流
//!
//! 唯一的生产入口是 `interactive_mode/commands/thinking.rs` 的 `show_thinking_selector`
//! （`/thinking` 无参数分支）：宿主传入会话当前等级、**当前模型可用**的等级集合、settings
//! 的默认等级与三个回调。可用集合在构造时被 clone 进组件（`all_items`），是打开瞬间的
//! 快照——会话中途切换模型不会刷新已打开的选择器。
//!
//! # 输入路由（状态机的进入顺序）
//!
//! 本组件自身实现 `Focusable`（TS `implements Focusable`），宿主把焦点给组件而不是内部
//! 列表；所有按键先进 [`ThinkingSelectorComponent::handle_input`]，按序分流：
//!
//! 1. `app.thinking.save`（仅当 `on_select_as_default` 存在）→ 以当前选中项触发
//!    "设为默认"回调，**不**进入搜索框；
//! 2. 四个导航键（`tui.select.up` / `down` / `confirm` / `cancel`）→ 原样转交
//!    `SelectList`；
//! 3. 其余按键 → 写入搜索框，随后按框内文本对 `all_items` 做 `fuzzy_filter` 原地重建
//!    列表（过滤后保留原选中项，若它仍在结果里）。
//!
//! # 边界与失败模式
//!
//! - 搜索框 `on_submit` 会向列表注入确认键：这意味着在搜索框里按提交键等于确认当前
//!   选中项，而不是提交搜索文本。
//! - 列表选中项若被过滤掉，重建后回到过滤结果的第一项；`level_from_str` 解析失败时
//!   回调静默不触发（列表项的 value 全部来自 `level_str`，正常不会发生）。
//! - 取消回调只关闭选择器、不改变任何等级；两个选择回调（普通选择 / 设为默认）由宿主
//!   决定是否持久化。

use std::any::Any;
use std::cell::{Ref, RefCell, RefMut};
use std::rc::Rc;

use kiana_agent_core::ThinkingLevel;
use kiana_theme::ThemeColor;
use kiana_tui::components::input::Input;
use kiana_tui::components::select_list::{SelectItem, SelectList, SelectListLayoutOptions};
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::Text;
use kiana_tui::fuzzy::fuzzy_filter;
use kiana_tui::keybindings::get_keybindings;
use kiana_tui::tui::{Component, Container, Focusable, TuiMouseEvent, TuiMouseEventResult};
use kiana_ui_kit::dynamic_border::DynamicBorder;
use kiana_ui_kit::keybinding_hints::key_display_text;

fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// `THINKING_SELECT_LIST_LAYOUT`。
fn layout() -> SelectListLayoutOptions {
    SelectListLayoutOptions {
        min_primary_column_width: Some(12.0),
        max_primary_column_width: Some(32.0),
        truncate_primary: None,
    }
}

/// 思维等级的字面量（`ModelThinkingLevel` 的 serde 小写形式）。
///
/// 列表项的 `value` 就是这个字面量；宿主用它做 `/thinking <level>` 的小写精确匹配，两边
/// 必须保持一致。
fn level_str(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "off",
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::Xhigh => "xhigh",
        ThinkingLevel::Max => "max",
    }
}

fn level_from_str(value: &str) -> Option<ThinkingLevel> {
    // 与 level_str 互为逆映射；新增 ThinkingLevel 变体时两处要同步。
    match value {
        "off" => Some(ThinkingLevel::Off),
        "minimal" => Some(ThinkingLevel::Minimal),
        "low" => Some(ThinkingLevel::Low),
        "medium" => Some(ThinkingLevel::Medium),
        "high" => Some(ThinkingLevel::High),
        "xhigh" => Some(ThinkingLevel::Xhigh),
        "max" => Some(ThinkingLevel::Max),
        _ => None,
    }
}

/// `LEVEL_DESCRIPTIONS`（**【逐字】**）。
fn level_description(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "No reasoning",
        ThinkingLevel::Minimal => "Very brief reasoning (~1k tokens)",
        ThinkingLevel::Low => "Light reasoning (~2k tokens)",
        ThinkingLevel::Medium => "Moderate reasoning (~8k tokens)",
        ThinkingLevel::High => "Deep reasoning (~16k tokens)",
        ThinkingLevel::Xhigh => "Extra-high reasoning (~32k tokens)",
        ThinkingLevel::Max => "Maximum reasoning",
    }
}

/// 选择回调（`onSelect`）。
type ThinkingCallback = Rc<RefCell<Box<dyn FnMut(ThinkingLevel)>>>;
/// 取消回调（`onCancel`）。
type CancelCallback = Rc<RefCell<Box<dyn FnMut()>>>;

/// 共享 [`Input`] 的代理。
struct InputProxy(Rc<RefCell<Input>>);

impl Component for InputProxy {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.0.borrow_mut().handle_mouse(event)
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for InputProxy {
    fn focused(&self) -> bool {
        self.0.borrow().focused()
    }

    fn set_focused(&mut self, focused: bool) {
        self.0.borrow_mut().set_focused(focused);
    }
}

/// 共享 [`SelectList`] 的代理（过滤时原地替换列表实例）。
struct ListProxy(Rc<RefCell<SelectList>>);

impl Component for ListProxy {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.0.borrow_mut().handle_mouse(event)
    }

    fn handles_input(&self) -> bool {
        true
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }
}

/// 思维等级选择器（TS `class ThinkingSelectorComponent extends Container implements Focusable`）。
pub struct ThinkingSelectorComponent {
    container: Container,
    search_input: Rc<RefCell<Input>>,
    select_list: Rc<RefCell<SelectList>>,
    all_items: Vec<SelectItem>,
    on_select: ThinkingCallback,
    on_cancel: CancelCallback,
    on_select_as_default: Option<ThinkingCallback>,
    focused: bool,
}

impl ThinkingSelectorComponent {
    /// TS `constructor(currentLevel, availableLevels, onSelect, onCancel, onSelectAsDefault?, defaultThinkingLevel?)`。
    ///
    /// `current_level` 决定哪一项带 `✓ ` 前缀；`default_thinking_level` 只影响
    /// description 的 `· default` 后缀，不参与预选。回调在组件内以
    /// `Rc<RefCell<Box<dyn FnMut>>>` 保存，因为列表重建（`apply_filter`）时会向新
    /// `SelectList` 实例重复安装同一对回调。
    pub fn new(
        current_level: ThinkingLevel,
        available_levels: Vec<ThinkingLevel>,
        on_select: impl FnMut(ThinkingLevel) + 'static,
        on_cancel: impl FnMut() + 'static,
        on_select_as_default: Option<Box<dyn FnMut(ThinkingLevel)>>,
        default_thinking_level: Option<ThinkingLevel>,
    ) -> Self {
        let on_select: ThinkingCallback = Rc::new(RefCell::new(Box::new(on_select)));
        let on_cancel: CancelCallback = Rc::new(RefCell::new(Box::new(on_cancel)));
        let on_select_as_default =
            on_select_as_default.map(|callback| Rc::new(RefCell::new(callback)));

        let all_items: Vec<SelectItem> = available_levels
            .iter()
            .map(|&level| {
                let description = if Some(level) == default_thinking_level {
                    format!("{} · default", level_description(level))
                } else {
                    level_description(level).to_string()
                };
                SelectItem {
                    value: level_str(level).to_string(),
                    label: format!(
                        "{}{}",
                        if level == current_level { "✓ " } else { "  " },
                        level_str(level)
                    ),
                    description: Some(description),
                }
            })
            .collect();

        let mut container = Container::new();
        container.add_child(Box::new(DynamicBorder::default()));
        container.add_child(Box::new(Spacer::new()));
        container.add_child(Box::new(Text::with_padding("Thinking Level", 0, 0)));
        container.add_child(Box::new(Spacer::new()));
        container.add_child(Box::new(Text::with_padding(
            format!(
                "{} cycles thinking levels in-session",
                key_display_text("app.thinking.cycle")
            ),
            0,
            0,
        )));
        container.add_child(Box::new(Spacer::new()));

        let search_input = Rc::new(RefCell::new(Input::new(Default::default())));
        container.add_child(Box::new(InputProxy(Rc::clone(&search_input))));
        container.add_child(Box::new(Spacer::new()));

        let select_list = Rc::new(RefCell::new(Self::build_select_list(
            &all_items,
            Some(current_level),
            Rc::clone(&on_select),
            Rc::clone(&on_cancel),
        )));
        // TS `this.searchInput.onSubmit = () => this.selectList.handleInput("\r")`：
        // 提交键（`tui.input.submit` 或字面量 `\n`）交给当前列表确认。
        {
            let select_list = Rc::clone(&select_list);
            search_input.borrow_mut().on_submit = Some(Box::new(move |_value: &str| {
                select_list.borrow_mut().handle_input("\r");
            }));
        }
        container.add_child(Box::new(ListProxy(Rc::clone(&select_list))));
        container.add_child(Box::new(Spacer::new()));
        container.add_child(Box::new(Text::with_padding(
            fg(
                ThemeColor::Dim,
                &format!(
                    "  {} to select · {} to set as default · {} to cancel",
                    key_display_text("tui.select.confirm"),
                    key_display_text("app.thinking.save"),
                    key_display_text("tui.select.cancel")
                ),
            ),
            0,
            0,
        )));
        container.add_child(Box::new(DynamicBorder::default()));

        ThinkingSelectorComponent {
            container,
            search_input,
            select_list,
            all_items,
            on_select,
            on_cancel,
            on_select_as_default,
            focused: false,
        }
    }

    fn build_select_list(
        items: &[SelectItem],
        preselect: Option<ThinkingLevel>,
        on_select: ThinkingCallback,
        on_cancel: CancelCallback,
    ) -> SelectList {
        let mut list = SelectList::new(
            items.to_vec(),
            usize::max(1, items.len()),
            kiana_theme::get_select_list_theme(),
            layout(),
        );
        if let Some(preselect) = preselect {
            let preselect = level_str(preselect);
            if let Some(index) = items.iter().position(|item| item.value == preselect) {
                list.set_selected_index(index as isize);
            }
        }
        list.on_select = Some(Box::new(move |item: &SelectItem| {
            if let Some(level) = level_from_str(&item.value) {
                (on_select.borrow_mut())(level);
            }
        }));
        list.on_cancel = Some(Box::new(move || {
            (on_cancel.borrow_mut())();
        }));
        list
    }

    fn apply_filter(&mut self, query: &str) {
        // 过滤 = 用匹配子集原地换一个新 SelectList 实例（TS 的 getSelectList 在过滤后
        // 也返回新实例）。重建时把"当前选中项"重新定位进去：先把选中项解析回
        // ThinkingLevel，再让 build_select_list 在新集合里按 value 找回；找不到就落在
        // 新集合的第 0 项。
        let filtered = if query.is_empty() {
            self.all_items.clone()
        } else {
            fuzzy_filter(self.all_items.clone(), query, |item| {
                format!(
                    "{} {}",
                    item.value,
                    item.description.as_deref().unwrap_or("")
                )
            })
        };
        let selected_value = self
            .select_list
            .borrow()
            .get_selected_item()
            .and_then(|item| level_from_str(&item.value));
        let new_list = Self::build_select_list(
            &filtered,
            selected_value,
            Rc::clone(&self.on_select),
            Rc::clone(&self.on_cancel),
        );
        *self.select_list.borrow_mut() = new_list;
    }

    /// TS `getSelectList()`。
    pub fn get_select_list(&self) -> Ref<'_, SelectList> {
        self.select_list.borrow()
    }

    /// 可变版本（测试直接驱动列表）。
    pub fn get_select_list_mut(&mut self) -> RefMut<'_, SelectList> {
        self.select_list.borrow_mut()
    }

    /// 当前搜索文本（测试用）。
    pub fn search_value(&self) -> String {
        self.search_input.borrow().get_value().to_string()
    }
}

impl Component for ThinkingSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn handle_input(&mut self, key_data: &str) {
        // 输入分流的三段顺序见模块文档"输入路由"；save 必须先于 nav 判断，否则同一
        // 按键若同时绑定导航会被列表先吃掉。
        let kb = get_keybindings();
        if kb.matches(key_data, "app.thinking.save")
            && let Some(callback) = &self.on_select_as_default
        {
            let level = self
                .select_list
                .borrow()
                .get_selected_item()
                .and_then(|item| level_from_str(&item.value));
            if let Some(level) = level {
                (callback.borrow_mut())(level);
            }
            return;
        }

        let is_nav = kb.matches(key_data, "tui.select.up")
            || kb.matches(key_data, "tui.select.down")
            || kb.matches(key_data, "tui.select.confirm")
            || kb.matches(key_data, "tui.select.cancel");
        if is_nav {
            self.select_list.borrow_mut().handle_input(key_data);
            return;
        }

        self.search_input.borrow_mut().handle_input(key_data);
        let query = self.search_input.borrow().get_value().to_string();
        self.apply_filter(&query);
    }

    fn handles_input(&self) -> bool {
        true
    }

    /// TS `Container.handleMouse`：按行把事件交给 search Input / SelectList。
    ///
    /// `Container` 自身没有 `handleInput`，所以命中的子控件请求焦点时结果里的
    /// `focus_target` 仍指向子控件（`tui.ts:397`）；外层 `dispatch_mouse_event` 依据本组件
    /// 的 `handles_input()` 决定是否把焦点收回本组件。
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.container.handle_mouse(event)
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        Some(&self.container.children)
    }

    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        Some(&mut self.container.children)
    }

    fn invalidate(&mut self) {
        self.container.invalidate();
    }
}

impl Focusable for ThinkingSelectorComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        self.search_input.borrow_mut().set_focused(focused);
    }
}
