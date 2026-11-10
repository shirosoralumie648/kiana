//! components/show-images-selector.ts。
//!
//! `/show-images` 的 Yes/No 选择器。
//!
//! 布局为 `DynamicBorder` + `SelectList(5)` + `DynamicBorder`，**无 Spacer**（保持紧凑）；
//! `getSelectList()` 公开。`SelectList` 作为容器的真实子项挂载（TS 里 `addChild(list)`），
//! 因此鼠标事件（`Container.handleMouse`）能按行路由到列表。
//!
//! ## 职责与执行流程
//!
//! 在“终端内联显示图片”（`yes`）与“只显示文本占位符”（`no`）之间二选一：构造时按
//! `current_value` 预选，`on_select` 只把条目的 `value` 映射成布尔（不读标签文本）。
//!
//! ## 约束与失败模式
//!
//! * 列表必须是容器的真实子项：鼠标事件只沿子项按行命中，若列表仅用于 `render` 拼装，
//!   点击与滚轮永远到不了它（测试
//!   `show_images_selector_forwards_clicks_to_the_select_list` 锁定该行为）。
//! * 本组件不处理键盘（`handles_input` 为 `false`）：焦点落在 [`SelectList`] 后由它消费
//!   方向键；`ListProxy` 只做转发。
//! * 列表用 `Rc<RefCell<_>>` 同时挂在容器里并暴露给 `get_select_list`，二者是同一个实例。
//! * 本仓库内目前只有测试直接构造该组件；公开再导出见 `components::index`。
//!
//! ## 为什么用代理而不是直接持有列表
//!
//! TS 里 `this.selectList` 与 `Container.children` 中的那一项是同一个对象，鼠标事件由
//! 容器按行命中后直接调用该对象。Rust 的 `Component` 以 `Box<dyn Component>` 独占所有权
//! 存进容器，无法同时把一个 `SelectList` 既放进 `children` 又留给 `get_select_list`，因此
//! 用 `Rc<RefCell<SelectList>>` 共享，并要求容器里放 `ListProxy` 来转发。
//!
//! ## 借用约定
//!
//! [`ShowImagesSelectorComponent::get_select_list`] / `get_select_list_mut` 返回的是
//! `Ref` / `RefMut` 守卫而非拷贝。守卫存活期间再调用本组件任何会借用同一列表的方法
//! （例如 `render`、`handle_mouse`）都会因 `RefCell` 重复借用而 panic；调用方应先取所需
//! 的值，再释放守卫。

use std::any::Any;
use std::cell::{Ref, RefCell, RefMut};
use std::rc::Rc;

use kiana_tui::components::select_list::{SelectItem, SelectList, SelectListLayoutOptions};
use kiana_tui::tui::{Component, Container, TuiMouseEvent, TuiMouseEventResult};
use kiana_ui_kit::dynamic_border::DynamicBorder;

/// `SHOW_IMAGES_SELECT_LIST_LAYOUT`。
///
/// 主列宽度限制在 12..=32 列；`truncate_primary: None` 表示使用 `SelectList` 的默认截断。
fn layout() -> SelectListLayoutOptions {
    SelectListLayoutOptions {
        min_primary_column_width: Some(12.0),
        max_primary_column_width: Some(32.0),
        truncate_primary: None,
    }
}

/// 共享 `SelectList` 的容器代理（TS 里列表就是 `Container.children` 的一项）。
///
/// 外层组件在 `select_list` 字段里保留同一个 `Rc`，供宿主读取/驱动列表；代理只负责把
/// `Component` 的渲染、鼠标、按键与失效调用转发过去。代理不覆写 `handles_input`，因此沿用
/// trait 默认值 `true`（与 TS 里“列表本身有 `handleInput`”一致）；外层的
/// [`ShowImagesSelectorComponent`] 则显式返回 `false`。
struct ListProxy(Rc<RefCell<SelectList>>);

impl Component for ListProxy {
    /// 渲染共享列表（不额外加边框或留白）。
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }

    /// 容器按行命中到代理后，把事件原样交给列表；列表内部再用局部坐标判断行。
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.0.borrow_mut().handle_mouse(event)
    }

    /// TS：点击列表后焦点落在 `SelectList` 本身（容器没有 `handleInput`），
    /// 之后方向键直接驱动列表。代理必须把按键转发给共享列表。
    fn handle_input(&mut self, key_data: &str) {
        self.0.borrow_mut().handle_input(key_data);
    }

    /// 暴露自身以便宿主/测试按类型下转（代理没有其它可观察状态）。
    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    /// 失效共享列表的渲染缓存。
    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }
}

/// 图片显示选择器（TS `class ShowImagesSelectorComponent extends Container`）。
///
/// `container` 负责边框与子项布局，`select_list` 与容器里的 `ListProxy` 指向同一个列表。
pub struct ShowImagesSelectorComponent {
    container: Container,
    select_list: Rc<RefCell<SelectList>>,
}

impl ShowImagesSelectorComponent {
    /// TS `constructor(currentValue, onSelect, onCancel)`。
    ///
    /// `current_value == true` 时预选 `yes`（下标 0），否则预选 `no`（下标 1）。两个条目的
    /// `value` 分别是 `"yes"` / `"no"`；`on_select` 在确认时收到 `value == "yes"` 的布尔值，
    /// `on_cancel` 在取消时调用。回调被移动进列表，之后不能替换。
    ///
    /// 列表最多显示 5 行（TS 的 `new SelectList(items, 5, …)` 传入的 `max_visible`），只有
    /// 两个条目时实际渲染两行。构造顺序必须是“上边框 → 列表代理 → 下边框”，
    /// `handle_mouse` 按这个顺序累加高度定位命中行。
    pub fn new(
        current_value: bool,
        mut on_select: impl FnMut(bool) + 'static,
        on_cancel: impl FnMut() + 'static,
    ) -> Self {
        let items = vec![
            SelectItem {
                value: "yes".to_string(),
                label: "Yes".to_string(),
                description: Some("Show images inline in terminal".to_string()),
            },
            SelectItem {
                value: "no".to_string(),
                label: "No".to_string(),
                description: Some("Show text placeholder instead".to_string()),
            },
        ];

        let mut container = Container::new();
        container.add_child(Box::new(DynamicBorder::default()));

        let mut select_list =
            SelectList::new(items, 5, kiana_theme::get_select_list_theme(), layout());
        select_list.set_selected_index(if current_value { 0 } else { 1 });
        select_list.on_select = Some(Box::new(move |item: &SelectItem| {
            on_select(item.value == "yes")
        }));
        select_list.on_cancel = Some(Box::new(on_cancel));

        let select_list = Rc::new(RefCell::new(select_list));
        container.add_child(Box::new(ListProxy(Rc::clone(&select_list))));
        container.add_child(Box::new(DynamicBorder::default()));

        ShowImagesSelectorComponent {
            container,
            select_list,
        }
    }

    /// TS `getSelectList()`。
    ///
    /// 返回共享列表的只读借用守卫，反射的是容器里那个 `ListProxy` 的同一实例。
    pub fn get_select_list(&self) -> Ref<'_, SelectList> {
        self.select_list.borrow()
    }

    /// 可变版本（测试/宿主直接驱动列表）。同样与容器里的代理共享同一实例。
    pub fn get_select_list_mut(&mut self) -> RefMut<'_, SelectList> {
        self.select_list.borrow_mut()
    }
}

impl Component for ShowImagesSelectorComponent {
    /// 渲染内部容器（边框 + 列表 + 边框）。
    fn render(&mut self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    /// TS `Container.handleMouse`（本组件没有 `handleInput`）。
    ///
    /// 委托给内部 `Container` 的 `handle_mouse`：它按子项实际渲染高度逐行匹配，命中后
    /// 把事件交给 `ListProxy`，从而真正改变列表选中项并触发 `on_select`。
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.container.handle_mouse(event)
    }

    fn handles_input(&self) -> bool {
        // TS：`ShowImagesSelectorComponent` 没有 `handleInput`。宿主据此不把键盘输入交给
        // 本组件；选择由点击（`handle_mouse`）或外部直接驱动列表完成。
        false
    }

    /// 暴露自身以便宿主按类型下转（组件本身没有其它可观察状态）。
    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    /// 暴露容器的直接子项（上边框、`ListProxy`、下边框），供宿主做焦点/事件遍历。
    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        Some(&self.container.children)
    }

    /// 可变版本，同上。
    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        Some(&mut self.container.children)
    }

    /// 向下传递给内部容器（它会失效所有子项并清空鼠标命中缓存）。
    fn invalidate(&mut self) {
        self.container.invalidate();
    }
}
