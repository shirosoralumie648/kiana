//! components/theme-selector.ts。
//!
//! 主题选择器：`SelectList` 直接列出 `getAvailableThemes()`，选中项描述为 `(current)`。
//!
//! 布局只有 `DynamicBorder` + `SelectList` + `DynamicBorder`，**无 Spacer**（保持紧凑）；
//! `getSelectList()` 公开。`SelectList` 作为容器的真实子项挂载（TS 里 `addChild(list)`），
//! 因此鼠标事件（`Container.handleMouse`）能按行路由到列表。
//!
//! # 调用方与回调
//!
//! 组件本身不持久化任何选择：三个回调（`on_select` 确认、`on_cancel` 取消、
//! `on_preview` 光标移动）全部交给宿主。`on_preview` 在**每次光标移动**时都会带新主题名
//! 触发，按 `spec/ca-interactive-components.md` §2.11 的描述由宿主调用 `initTheme` 做
//! 实时预览；确认与取消则由 `SelectList` 的 `on_select` / `on_cancel` 转发。组件内不
//! 保存"进入时的主题"，预览之后如何恢复由宿主决定。
//!
//! # 为什么用 ListProxy
//!
//! `SelectList` 是共享状态（`Rc<RefCell<..>>`）：容器通过私有的 `ListProxy` 渲染并转发
//! 输入，宿主通过 [`ThemeSelectorComponent::get_select_list`] 直读/直写同一实例，两条
//! 路径看到的选中项与过滤状态一致。若列表改为容器私有的子项，宿主拿到的快照就会与
//! 容器里实际渲染的列表脱节。
//!
//! 本组件 `handles_input()` 为 `false`（TS 没有 `handleInput`）：按键交互全部由焦点
//! 落在 `SelectList` 后的列表自身处理，`ListProxy` 的 `handle_input` 只在焦点已被
//! 容器路由到列表时才会被调到。

use std::any::Any;
use std::cell::{Ref, RefCell, RefMut};
use std::rc::Rc;

use kiana_theme::get_available_themes;
use kiana_tui::components::select_list::{SelectItem, SelectList, SelectListLayoutOptions};
use kiana_tui::tui::{Component, Container, TuiMouseEvent, TuiMouseEventResult};
use kiana_ui_kit::dynamic_border::DynamicBorder;

/// `THEME_SELECT_LIST_LAYOUT`。
fn layout() -> SelectListLayoutOptions {
    SelectListLayoutOptions {
        min_primary_column_width: Some(12.0),
        max_primary_column_width: Some(32.0),
        truncate_primary: None,
    }
}

/// 共享 [`SelectList`] 的容器代理（TS 里列表就是 `Container.children` 的一项）。
struct ListProxy(Rc<RefCell<SelectList>>);

impl Component for ListProxy {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.0.borrow_mut().handle_mouse(event)
    }

    /// TS：点击列表后焦点落在 `SelectList` 本身（容器没有 `handleInput`），
    /// 之后方向键直接驱动列表。代理必须把按键转发给共享列表。
    fn handle_input(&mut self, key_data: &str) {
        self.0.borrow_mut().handle_input(key_data);
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }
}

/// 主题选择器（TS `class ThemeSelectorComponent extends Container`）。
pub struct ThemeSelectorComponent {
    container: Container,
    select_list: Rc<RefCell<SelectList>>,
}

impl ThemeSelectorComponent {
    /// TS `constructor(currentTheme, onSelect, onCancel, onPreview)`。
    ///
    /// 列表项即全部可用主题名（label 就是名字本身，无 `✓` 前缀；当前主题只体现在
    /// description `(current)` 上）。构造时把选中项定位到 `current_theme`：不在列表里时
    /// 跳过定位，`SelectList` 保持默认的 0 号项。`max_visible = 10`。
    pub fn new(
        current_theme: &str,
        mut on_select: impl FnMut(&str) + 'static,
        on_cancel: impl FnMut() + 'static,
        mut on_preview: impl FnMut(&str) + 'static,
    ) -> Self {
        let themes = get_available_themes();
        let items: Vec<SelectItem> = themes
            .iter()
            .map(|name| SelectItem {
                value: name.clone(),
                label: name.clone(),
                description: if name == current_theme {
                    Some("(current)".to_string())
                } else {
                    None
                },
            })
            .collect();

        let mut container = Container::new();
        container.add_child(Box::new(DynamicBorder::default()));

        let mut select_list =
            SelectList::new(items, 10, kiana_theme::get_select_list_theme(), layout());
        let current_index = themes.iter().position(|name| name == current_theme);
        if let Some(current_index) = current_index {
            select_list.set_selected_index(current_index as isize);
        }

        select_list.on_select = Some(Box::new(move |item: &SelectItem| on_select(&item.value)));
        select_list.on_cancel = Some(Box::new(on_cancel));
        select_list.on_selection_change =
            Some(Box::new(move |item: &SelectItem| on_preview(&item.value)));

        let select_list = Rc::new(RefCell::new(select_list));
        container.add_child(Box::new(ListProxy(Rc::clone(&select_list))));
        container.add_child(Box::new(DynamicBorder::default()));

        ThemeSelectorComponent {
            container,
            select_list,
        }
    }

    /// TS `getSelectList()`。
    ///
    /// 借用共享列表；宿主读取选中项/条目用。与 `on_preview` 配合可以实现"确认前实时
    /// 预览"（见模块文档）。注意持有 `Ref` 期间不能调用本组件的其它方法。
    pub fn get_select_list(&self) -> Ref<'_, SelectList> {
        self.select_list.borrow()
    }

    /// 可变版本（测试/宿主直接驱动列表）。
    pub fn get_select_list_mut(&mut self) -> RefMut<'_, SelectList> {
        self.select_list.borrow_mut()
    }
}

impl Component for ThemeSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    /// TS `Container.handleMouse`（本组件没有 `handleInput`）。
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.container.handle_mouse(event)
    }

    fn handles_input(&self) -> bool {
        // TS：`ThemeSelectorComponent` 没有 `handleInput`。
        false
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
