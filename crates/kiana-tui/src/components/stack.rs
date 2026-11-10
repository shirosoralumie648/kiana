//! components/stack.ts。
//!
//! 栈布局基类：持有 [`StackLayoutEntry`]（布局协议）与一份容器子项列表（焦点/挂载遍历），
//! 并提供 `allocateStackSizes` / `visibleStackEntries` 的再导出（实现在
//! [`crate::layout_node`]，布局器与本模块共用同一份）。
//!
//! ## 与 TS 的表示差异
//!
//! 1. TS 用鸭子判定 `"render" in child` 区分 `Component | StackEntry`；Rust 用显式枚举
//!    [`StackChild`]（另加 [`StackChild::Shared`] 便于把已有的 `Rc<RefCell<dyn Component>>`
//!    直接挂进栈）。
//! 2. TS 的 `entries[i].component` 与 `container.children[i]` 是**同一个对象**。Rust 的
//!    [`crate::tui::Component::render`] 需要 `&mut self`，布局器又只能持有
//!    [`crate::layout_node::SharedComponent`]（`Rc<RefCell<dyn Component>>`），所以每个子项被包成
//!    `SharedHandle`：布局条目持有 `Rc<RefCell<SharedHandle>>`，容器子项持有共享同一
//!    `inner`/`focused` 的 `Box<SharedHandle>` 克隆。两条路径渲染的是同一个底层组件，
//!    `focused` 标志也共享（渲染/输入前同步给底层组件的 [`Focusable`]）。代理把
//!    [`crate::tui::Component::for_each_container_child`] 透传给底层组件，因此焦点路径、
//!    `containsComponent` 与鼠标焦点解析能穿过任意层 Stack/ScrollView（TS 里这些容器的
//!    `children` 就是真实子对象）。
//! 3. `[LAYOUT_NODE]()` 在 TS 返回**同一个** entries 数组；Rust 的 [`crate::layout_node`]
//!    用 `Vec` 所有，只能克隆一份快照（布局器每次布局都重新取节点，语义等价）。
//! 4. TS 的 `removeChild(component)` 按引用比较；Rust 只能按指针比较调用方手里那个
//!    `&dyn Component`（即 [`crate::tui::Component::container_children`] 暴露的句柄）。
//!
//! ## 职责
//!
//! [`Stack`] 是 VStack/HStack 的共同实现：保存与 TS `StackLayoutEntry[]` 平行的条目
//! 列表，并向布局器暴露 `layout_node`。它自身不实现 [`crate::tui::Component`]：垂直
//! 渲染由 [`crate::components::v_stack::VStack`] 复用 `render_vertical`，水平渲染由
//! [`crate::components::h_stack::HStack`] 实现；两者的鼠标处理共用
//! `handle_container_mouse`（`ScrollView` 也调用它）。
//!
//! ## 不变量
//!
//! `entries` 与 `children` 必须等长、同序：第 i 个条目与第 i 个容器子项包装同一个底层
//! 组件。所有公开的增删方法都同时维护两份列表；绕过它们直接修改
//! `container_children_mut` 返回的 `Vec` 会破坏该对应关系。
//!
//! ## 垂直布局流程
//!
//! 1. `visible_stack_entries` 按 `visible(viewport)` 过滤条目；
//! 2. 每个可见子项以视口宽度渲染，行数即内在高度；
//! 3. `allocate_stack_sizes(..., None, gap)` 处于测量模式：只按 basis/min/max 夹取，
//!    `grow`/`shrink` 不参与；
//! 4. 按 `gap` 插入空行；每项取前 `size` 行，不足补空行。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::layout_node::{
    LayoutNode, LayoutViewport, StackAlign, StackLayoutEntry, StackLayoutNode, StackLayoutType,
    VisibleFn,
};
pub use crate::layout_node::{allocate_stack_sizes, visible_stack_entries};
use crate::tui::{Component, Focusable, TuiMouseEvent, TuiMouseEventResult};

/// 条目选项（TS `StackEntryOptions`）。
///
/// 字段为 `None` 表示 TS 的 `undefined`（不写入条目，由分配器按默认值处理）。
#[derive(Clone, Default)]
pub struct StackEntryOptions {
    /// 固定基准尺寸；`None` 表示未指定，由分配器按内容自动确定。
    ///
    /// 值不在这里归一化：分配器 `allocate_stack_sizes` 会再做 `floor` 与不小于 0 的夹取。
    pub basis: Option<f64>,
    /// 剩余空间的分配权重；`None` 表示默认值 `0`（不参与 grow）。
    pub grow: Option<f64>,
    /// 空间不足时的收缩权重；`None` 表示默认值 `1`（可收缩）。
    pub shrink: Option<f64>,
    /// 最小尺寸；`None` 表示默认值 `0`。
    pub min_size: Option<f64>,
    /// 最大尺寸；`None` 表示默认值 `MAX_SAFE_INTEGER`（2^53-1）。
    pub max_size: Option<f64>,
    /// 可见性回调（TS `visible?: (viewport) => boolean`）。
    pub visible: Option<VisibleFn>,
}

impl std::fmt::Debug for StackEntryOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StackEntryOptions")
            .field("basis", &self.basis)
            .field("grow", &self.grow)
            .field("shrink", &self.shrink)
            .field("min_size", &self.min_size)
            .field("max_size", &self.max_size)
            .field("visible", &self.visible.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

/// 带选项的子项（TS `StackEntry`）。
pub struct StackEntry {
    /// 要挂入栈的组件；`Stack` 会把它包装成共享句柄。
    pub component: Box<dyn Component>,
    /// 该子项的布局选项。
    pub options: StackEntryOptions,
}

impl StackEntry {
    /// 只有组件的条目（选项全默认）。
    pub fn new(component: Box<dyn Component>) -> Self {
        StackEntry {
            component,
            options: StackEntryOptions::default(),
        }
    }

    /// 带选项的条目。
    pub fn with_options(component: Box<dyn Component>, options: StackEntryOptions) -> Self {
        StackEntry { component, options }
    }
}

/// 栈子项（TS `StackChild = Component | StackEntry`）。
pub enum StackChild {
    /// 普通组件（选项全默认）。
    Component(Box<dyn Component>),
    /// 已共享的组件句柄。
    Shared(crate::layout_node::SharedComponent),
    /// 已共享的组件句柄 + 布局选项。
    SharedEntry(crate::layout_node::SharedComponent, StackEntryOptions),
    /// 带布局选项的条目。
    Entry(StackEntry),
}

impl From<Box<dyn Component>> for StackChild {
    fn from(component: Box<dyn Component>) -> Self {
        StackChild::Component(component)
    }
}

impl StackChild {
    /// 带选项的便捷构造。
    pub fn entry(component: Box<dyn Component>, options: StackEntryOptions) -> Self {
        StackChild::Entry(StackEntry::with_options(component, options))
    }
}

/// 栈选项（TS `StackOptions`）。
#[derive(Clone, Default)]
pub struct StackOptions {
    /// 子项之间的空行数；构造时经 `normalize_size` 处理（`floor`、不小于 0，非有限值→0）。
    pub gap: Option<f64>,
    /// 交叉轴对齐；`None` 等价于 [`StackAlign::Stretch`]。
    pub align: Option<StackAlign>,
}

impl std::fmt::Debug for StackOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StackOptions")
            .field("gap", &self.gap)
            .field("align", &self.align)
            .finish()
    }
}

/// `normalizeSize(value, fallback)`：非有限值 → fallback；否则 `max(0, floor(value))`。
fn normalize_size(value: Option<f64>, fallback: f64) -> f64 {
    match value {
        Some(value) if value.is_finite() => value.floor().max(0.0),
        _ => fallback,
    }
}

/// 完全拥有的组件包装：把 `Box<dyn Component>` 变成可实现 `Unsize` 的定长类型，
/// 从而能放进 `Rc<RefCell<dyn Component>>`。
///
/// 所有 [`Component`] 方法直接转发给内部的 Box（`&mut self`/`&self` 的生命周期都来自
/// `self`，因此无需借用守卫）。
pub(crate) struct OwnedComponent(pub(crate) Box<dyn Component>);

impl Component for OwnedComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.render(width)
    }

    fn handle_input(&mut self, data: &str) {
        self.0.handle_input(data);
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.0.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        self.0.wants_key_release()
    }

    fn handles_input(&self) -> bool {
        self.0.handles_input()
    }

    fn invalidate(&mut self) {
        self.0.invalidate();
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        self.0.as_any()
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        self.0.as_any_mut()
    }

    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        self.0.container_children()
    }

    fn mouse_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        self.0.mouse_children_mut()
    }

    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        self.0.container_children_mut()
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        self.0.as_focusable()
    }

    fn layout_node(&self) -> Option<LayoutNode> {
        self.0.layout_node()
    }
}

/// 共享子项句柄：布局条目与容器子项共用同一底层组件与 `focused` 标志。
///
/// 渲染/输入前把 `focused` 同步给底层组件的 [`Focusable`]（TS 里 `focused` 是组件自身的
/// 公开字段，容器遍历直接写它；Rust 的 trait 对象无法把引用借出 [`RefCell`]，因此用
/// `Cell` 镜像并在下一入口同步）。
pub(crate) struct SharedHandle {
    inner: crate::layout_node::SharedComponent,
    focused: Rc<Cell<bool>>,
}

impl Clone for SharedHandle {
    fn clone(&self) -> Self {
        SharedHandle {
            inner: Rc::clone(&self.inner),
            focused: Rc::clone(&self.focused),
        }
    }
}

impl SharedHandle {
    /// 新句柄。
    ///
    /// 包装同一个共享组件；`focused` 从 `false` 开始。
    pub(crate) fn new(inner: crate::layout_node::SharedComponent) -> Self {
        SharedHandle {
            inner,
            focused: Rc::new(Cell::new(false)),
        }
    }

    /// 底层共享组件（布局条目与容器子项共用；备用屏用它比对组件身份）。
    pub(crate) fn inner(&self) -> &crate::layout_node::SharedComponent {
        &self.inner
    }

    /// 同步镜像的 `focused` 标志到底层组件。
    ///
    /// 底层不可聚焦（`as_focusable` 返回 `None`）时无操作。
    fn sync_focus(&self) {
        let focused = self.focused.get();
        if let Some(focusable) = self.inner.borrow_mut().as_focusable() {
            focusable.set_focused(focused);
        }
    }
}

impl Component for SharedHandle {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.sync_focus();
        self.inner.borrow_mut().render(width)
    }

    fn handle_input(&mut self, data: &str) {
        self.sync_focus();
        self.inner.borrow_mut().handle_input(data);
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.borrow_mut().handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        self.inner.borrow().wants_key_release()
    }

    fn handles_input(&self) -> bool {
        self.inner.borrow().handles_input()
    }

    fn invalidate(&mut self) {
        self.inner.borrow_mut().invalidate();
    }

    /// 返回 `Some(self)`：备用屏靠它把代理向下转型成 [`SharedHandle`]，再用 [`SharedHandle::inner`]
    /// 取底层组件做身份比较（见 `alt_screen.rs` 的 `ComponentIdentity::of` 与
    /// `component_underlying_address`：两者都用 `as_any()` 做不可变向下转型）。
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    /// [`Component::as_any`] 的可变版本，同样返回 `Some(self)`。
    ///
    /// 鼠标分发的重放路径用可变转型：`alt_screen.rs` 的 `shared_wrapper_inner` 通过
    /// `as_any_mut()` 向下转型 `SharedProxy` / `SharedHandle`，再取出底层句柄继续沿地址链
    /// 分发（见 `dispatch_along_path`）。
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    /// 底层组件可获得焦点时才返回 `Some(self)`；否则返回 `None`，避免把不可聚焦的
    /// 组件当成焦点目标。
    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        if self.inner.borrow_mut().as_focusable().is_some() {
            Some(self)
        } else {
            None
        }
    }

    /// 透明下钻：TS 里 `Stack`/`ScrollView` 的 `children` 就是真实子组件，焦点路径、
    /// `containsComponent`、`setFocus` 能穿过任意层嵌套。Rust 的子项是共享句柄代理，
    /// 因此这里把遍历透传给底层组件（`OwnedComponent` 的默认实现再转发到真实容器）。
    fn for_each_container_child(&self, f: &mut dyn FnMut(&dyn Component) -> bool) {
        self.inner.borrow().for_each_container_child(f);
    }

    /// [`Component::for_each_container_child`] 的可变版本。
    fn for_each_container_child_mut(
        &mut self,
        f: &mut dyn FnMut(&mut (dyn Component + 'static)) -> bool,
    ) {
        self.inner.borrow_mut().for_each_container_child_mut(f);
    }

    /// 转发底层组件的布局节点（底层每次调用重新构造，见 [`crate::layout_node`]）。
    fn layout_node(&self) -> Option<LayoutNode> {
        self.inner.borrow().layout_node()
    }
}

impl Focusable for SharedHandle {
    fn focused(&self) -> bool {
        self.focused.get()
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused.set(focused);
        // TS 的 `focused` 是组件自身的字段，TUI 写入后立即可见（`Input.render` 依赖它决定
        // 是否输出光标标记）。Rust 的句柄镜像必须立刻同步到底层组件，而不是等下一次入口。
        if let Some(focusable) = self.inner.borrow_mut().as_focusable() {
            focusable.set_focused(focused);
        }
    }
}

/// 把 `Box<dyn Component>` 包成"布局句柄 + 容器子项句柄"。
///
/// 布局句柄（`Rc<RefCell<SharedHandle>>`）就是 [`StackLayoutEntry::component`]；
/// 容器子项句柄（`Box<SharedHandle>`）供 [`Component::container_children`] 暴露。
pub(crate) fn wrap_child(
    component: Box<dyn Component>,
) -> (crate::layout_node::SharedComponent, Box<dyn Component>) {
    let inner: crate::layout_node::SharedComponent =
        Rc::new(RefCell::new(OwnedComponent(component)));
    wrap_shared(inner)
}

/// 同 [`wrap_child`]，但底层已经是共享句柄。
pub(crate) fn wrap_shared(
    shared: crate::layout_node::SharedComponent,
) -> (crate::layout_node::SharedComponent, Box<dyn Component>) {
    let handle = SharedHandle::new(shared);
    let layout_handle: crate::layout_node::SharedComponent = Rc::new(RefCell::new(handle.clone()));
    (layout_handle, Box::new(handle))
}

/// 栈基类（TS `abstract class Stack extends Container`）。
///
/// `layout_type` 由 [`crate::components::v_stack::VStack`] /
/// [`crate::components::h_stack::HStack`] 决定。
pub struct Stack {
    /// 布局条目（布局器读取的快照来源）。
    entries: Vec<StackLayoutEntry>,
    /// 与 `entries` 平行的容器子项（焦点/挂载遍历用）。
    pub(crate) children: Vec<Box<dyn Component>>,
    /// 已归一化的子项间距（整数行）。
    gap: f64,
    /// 交叉轴对齐方式（HStack 渲染时使用）。
    align: StackAlign,
    /// 该栈是垂直还是水平，由 VStack/HStack 构造时固定。
    layout_type: StackLayoutType,
}

impl Stack {
    /// 构造（`children` 依 TS 的 `StackChild[]` 语义逐个 `addChild`）。
    pub fn new(
        layout_type: StackLayoutType,
        children: Vec<StackChild>,
        options: StackOptions,
    ) -> Self {
        let mut stack = Stack {
            entries: Vec::new(),
            children: Vec::new(),
            gap: normalize_size(options.gap, 0.0),
            align: options.align.unwrap_or(StackAlign::Stretch),
            layout_type,
        };
        for child in children {
            match child {
                StackChild::Component(component) => stack.add_child(component),
                StackChild::Shared(shared) => stack.add_shared(shared),
                StackChild::SharedEntry(shared, options) => {
                    stack.add_shared_with_options(shared, options);
                }
                StackChild::Entry(entry) => {
                    stack.add_entry(entry.component, entry.options);
                }
            }
        }
        stack
    }

    /// 布局条目（只读）。
    pub fn entries(&self) -> &[StackLayoutEntry] {
        &self.entries
    }

    /// `gap` getter。
    pub fn gap(&self) -> f64 {
        self.gap
    }

    /// `align` getter。
    pub fn align(&self) -> StackAlign {
        self.align
    }

    /// 布局类型（`"vstack"` / `"hstack"` 的内部表示）。
    pub fn layout_type(&self) -> StackLayoutType {
        self.layout_type
    }

    /// `addChild(component)`（选项全默认）。
    pub fn add_child(&mut self, component: Box<dyn Component>) {
        self.add_entry(component, StackEntryOptions::default());
    }

    /// `addChild(component, options)`。
    pub fn add_child_with_options(
        &mut self,
        component: Box<dyn Component>,
        options: StackEntryOptions,
    ) {
        self.add_entry(component, options);
    }

    /// 挂入一个已共享的组件。
    pub fn add_shared(&mut self, shared: crate::layout_node::SharedComponent) {
        let (layout_handle, child) = wrap_shared(shared);
        self.push_entry(layout_handle, child, StackEntryOptions::default());
    }

    /// 挂入一个已共享的组件并带布局选项。
    pub fn add_shared_with_options(
        &mut self,
        shared: crate::layout_node::SharedComponent,
        options: StackEntryOptions,
    ) {
        let (layout_handle, child) = wrap_shared(shared);
        self.push_entry(layout_handle, child, options);
    }

    fn add_entry(&mut self, component: Box<dyn Component>, options: StackEntryOptions) {
        let (layout_handle, child) = wrap_child(component);
        self.push_entry(layout_handle, child, options);
    }

    /// 把包装后的句柄追加到两份平行列表。
    ///
    /// 选项只在 `Some` 时归一化，`None` 原样保留给分配器取 TS 默认值（grow `0`、
    /// shrink `1`、min `0`、max `MAX_SAFE_INTEGER`）；`basis` 不归一化。追加顺序与 TS
    /// 一致：先 `super.addChild`（`children`），再 `entries.push`。
    fn push_entry(
        &mut self,
        layout_handle: crate::layout_node::SharedComponent,
        child: Box<dyn Component>,
        options: StackEntryOptions,
    ) {
        self.children.push(child);
        self.entries.push(StackLayoutEntry {
            component: layout_handle,
            basis: options.basis,
            grow: options.grow.map(|value| normalize_size(Some(value), 0.0)),
            shrink: options.shrink.map(|value| normalize_size(Some(value), 1.0)),
            min_size: options
                .min_size
                .map(|value| normalize_size(Some(value), 0.0)),
            max_size: options
                .max_size
                .map(|value| normalize_size(Some(value), crate::layout_node::MAX_SAFE_INTEGER)),
            visible: options.visible,
        });
    }

    /// `removeChild(component)`：按指针移除第一个匹配项（同时删条目）。
    ///
    /// 比较的是容器子项句柄（`SharedHandle`）的地址，因此调用方应传入从
    /// [`Component::container_children`] 取得的引用，而不是最初传入的原始组件。
    /// 找不到时不改动任何列表。
    pub fn remove_child(&mut self, component: &dyn Component) {
        let target = component as *const dyn Component as *const () as usize;
        let index = self.children.iter().position(|child| {
            child.as_ref() as *const dyn Component as *const () as usize == target
        });
        if let Some(index) = index {
            self.children.remove(index);
            self.entries.remove(index);
        }
    }

    /// 按槽位下标移除。
    ///
    /// 同时删除 `entries` 与 `children` 的同一位置并返回子项；越界返回 `None`，
    /// 两份列表都不改动。
    pub fn remove_child_at(&mut self, index: usize) -> Option<Box<dyn Component>> {
        if index >= self.children.len() {
            return None;
        }
        self.entries.remove(index);
        Some(self.children.remove(index))
    }

    /// `clear()`。
    ///
    /// 同时清空布局条目与容器子项（与 TS `Stack.clear` 一致）。
    pub fn clear(&mut self) {
        self.children.clear();
        self.entries.clear();
    }

    /// `[LAYOUT_NODE]()`：每次调用重新构造（`entries` 为快照）。
    ///
    /// 返回的节点持有条目的克隆（含共享句柄的 `Rc`）；修改节点本身不会回写 `Stack`，
    /// 但通过共享句柄改组件状态对两边都可见。
    pub fn layout_node(&self) -> LayoutNode {
        LayoutNode::Stack(StackLayoutNode {
            layout_type: self.layout_type,
            entries: self.entries.clone(),
            gap: self.gap,
            align: self.align,
        })
    }

    /// VStack 的渲染算法（`components/v-stack.ts` 的 `render`）。
    pub(crate) fn render_vertical(&mut self, width: usize) -> Vec<String> {
        let viewport = LayoutViewport {
            width: width.max(1),
            height: crate::layout_node::MAX_SAFE_INTEGER as usize,
        };
        // 视口高度取 MAX_SAFE_INTEGER：VStack 只关心宽度，不做高度裁剪。
        let entries = visible_stack_entries(&self.entries, viewport);
        // 每个可见子项按视口宽度渲染一次，行数就是它的内在高度。
        let rendered: Vec<Vec<String>> = entries
            .iter()
            .map(|entry| entry.component.borrow_mut().render(viewport.width))
            .collect();
        let intrinsic: Vec<f64> = rendered.iter().map(|lines| lines.len() as f64).collect();
        // `available_size = None` 是测量模式：只按 basis/min/max 夹取，grow/shrink 不参与。
        let sizes = allocate_stack_sizes(&entries, &intrinsic, None, self.gap);
        let gap = self.gap as usize;
        let mut lines: Vec<String> = Vec::new();
        for (index, child_lines) in rendered.iter().enumerate() {
            // 相邻子项之间插入 `gap` 个空行（第一个子项前不插）。
            if index > 0 {
                for _ in 0..gap {
                    lines.push(String::new());
                }
            }
            // 分配尺寸可能小于内在高度（被 min/max 或 basis 夹过）：先取前 size 行。
            let size = sizes[index] as usize;
            let taken: Vec<String> = child_lines.iter().take(size).cloned().collect();
            // TS：`for (padding = childLines.length; padding < size; padding++) push("")`
            let padding = size.saturating_sub(taken.len());
            lines.extend(taken);
            for _ in 0..padding {
                lines.push(String::new());
            }
        }
        lines
    }
}

/// TS `Container.handleMouse` 的单容器版本（Stack/ScrollView 共用）。
///
/// TS 的 Stack/ScrollView 从不调用 `Container.render`，因此 `mouseLayout` 永远是
/// `undefined`：每次都现场以 `event.width` 渲染子项求高度。`focusTarget` 的"容器收回"
/// 分支要求容器自身有 `handleInput`，而 Stack/ScrollView 都没有，所以直接返回分发结果。
///
/// 命中判定只看 `event.y`：子项按渲染高度竖向切分，`event.x` 不参与。因此 HStack 中
/// 同一行的多个子项会命中第一个覆盖该 y 的项（TS 的 `Container.handleMouse` 同样只看 y）。
pub(crate) fn handle_container_mouse(
    children: &mut [Box<dyn Component>],
    event: &TuiMouseEvent,
) -> Option<TuiMouseEventResult> {
    // y 越界（负数或不小于 `event.height`）直接不处理。
    if event.y < 0 || event.y as usize >= event.height {
        return None;
    }
    // 现场渲染每个子项测高：TS 的 Stack 从不调用 Container.render，mouseLayout 不会
    // 缓存，因此每次鼠标事件都重新渲染（与 TS 相同）。
    let heights: Vec<usize> = children
        .iter_mut()
        .map(|child| child.render(event.width).len())
        .collect();
    // 按自上而下的顺序累加高度；命中后把 y 换成子项内坐标再分发。
    // 注意：累加高度不含 Stack 的 `gap` 空行，存在 gap 时第一个 gap 之后的命中区域会
    // 相对画面向上偏移（TS 的 Stack 继承同一个 Container.handleMouse，行为相同）。
    let mut child_y: isize = 0;
    for (child, height) in children.iter_mut().zip(heights) {
        if event.y >= child_y && event.y < child_y + height as isize {
            let mut child_event = event.clone();
            child_event.y = event.y - child_y;
            child_event.height = height;
            return crate::tui::dispatch_mouse_event(child.as_mut(), &child_event);
        }
        child_y += height as isize;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Lines(Rc<Vec<String>>);

    impl Component for Lines {
        fn render(&mut self, _width: usize) -> Vec<String> {
            (*self.0).clone()
        }
    }

    fn lines(values: &[&str]) -> Box<dyn Component> {
        Box::new(Lines(Rc::new(
            values.iter().map(|value| (*value).to_string()).collect(),
        )))
    }

    #[test]
    fn normalize_size_matches_ts() {
        assert_eq!(normalize_size(None, 0.0), 0.0);
        assert_eq!(normalize_size(Some(f64::NAN), 1.0), 1.0);
        assert_eq!(normalize_size(Some(f64::INFINITY), 1.0), 1.0);
        assert_eq!(normalize_size(Some(2.9), 0.0), 2.0);
        assert_eq!(normalize_size(Some(-3.0), 0.0), 0.0);
    }

    #[test]
    fn entries_share_component_with_container_children() {
        let stack = Stack::new(
            StackLayoutType::VStack,
            vec![StackChild::Component(lines(&["a"]))],
            StackOptions::default(),
        );
        let layout_handle = stack.entries()[0].component.clone();
        let rendered = layout_handle.borrow_mut().render(5);
        assert_eq!(rendered, ["a"]);
        assert_eq!(stack.children.len(), 1);
    }

    #[test]
    fn remove_child_at_removes_entry() {
        let mut stack = Stack::new(
            StackLayoutType::VStack,
            vec![
                StackChild::Component(lines(&["a"])),
                StackChild::Component(lines(&["b"])),
            ],
            StackOptions::default(),
        );
        let mut removed = stack.remove_child_at(0).expect("第一个子项存在");
        assert_eq!(removed.render(3), ["a"]);
        assert_eq!(stack.children.len(), 1);
        assert_eq!(stack.entries().len(), 1);
        assert_eq!(stack.entries()[0].component.borrow_mut().render(3), ["b"]);
        stack.clear();
        assert!(stack.entries().is_empty());
        assert!(stack.children.is_empty());
    }

    #[test]
    fn visible_stack_entries_filters() {
        let mut stack = Stack::new(
            StackLayoutType::VStack,
            vec![
                StackChild::Component(lines(&["one"])),
                StackChild::entry(
                    lines(&["hidden"]),
                    StackEntryOptions {
                        visible: Some(Rc::new(|_viewport| false)),
                        ..Default::default()
                    },
                ),
                StackChild::Component(lines(&["two"])),
            ],
            StackOptions::default(),
        );
        stack.gap = 1.0;
        assert_eq!(stack.render_vertical(10), ["one", "", "two"]);
    }
}
