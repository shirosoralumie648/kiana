//! components/mouse-region.ts。
//!
//! 给任意组件附加鼠标处理，而不改变它的渲染输出。
//!
//! # 职责与边界
//!
//! [`MouseRegion`] 是一个装饰器：持有一个子组件和一个外部处理器。`render` 与 `invalidate`
//! 直接转发给子组件，因此包装不改变布局与绘制；`handle_mouse` 先经 [`dispatch_mouse_event`]
//! 把事件交给子组件，子组件没有消费时才调用外部处理器。
//!
//! 事件**不做坐标重定位**：父级分发者投递时已经把 `x`/`y` 换算成相对本区域的坐标
//! （见 [`TuiMouseEvent`] 的字段说明），所以子组件与回调读到的是同一份局部坐标。
//!
//! 典型用法是"给纯展示组件加点击行为"：`kiana-interactive` 的消息组件（如
//! `compaction_summary_message`、`skill_invocation_message`）用它把整行文本变成可展开/收起
//! 的点击区域；`tests/tui_alt_screen.rs` 也用它记录鼠标事件并验证分发语义。
//!
//! # 注意事项
//!
//! - [`Component::handles_input`] 返回 `false`：TS 的 `MouseRegion` 没有 `handleInput`。
//!   当 `handle_mouse` 把携带子组件地址的分发结果原样上交时，[`dispatch_mouse_event`] 用
//!   这个标志决定 `focusTarget` 的归属：`false` 保留子组件地址，`true` 则会被改写成包装器
//!   自己的地址（焦点被收回包装器）。
//!   已知限制：`MouseRegion` 没有覆写 [`Component::container_children`] /
//!   [`Component::for_each_container_child`]，被包装的子组件不在挂载/焦点遍历里；上面保留
//!   的子组件地址因此通常解析不到（`TuiBase::focus_target_of_address` 返回 `None`），调用方
//!   会回退到本次分发的 fallback 目标。不要据此认为焦点一定会落到子组件上。
//! - 外部处理器返回的结果原样上交；若 `handled`/`capture`/`focus` 都不为 true，父级的
//!   [`dispatch_mouse_event`] 会把它当作未处理而继续冒泡。

use crate::tui::{Component, TuiMouseEvent, TuiMouseEventResult, dispatch_mouse_event};

/// 鼠标处理回调（TS `MouseRegionHandler`）。
///
/// 只有子组件没有消费事件时才会被调用；返回 `None` 表示继续向外冒泡，返回
/// `Some(result)` 则作为本区域的结果交给上层分发者。
pub type MouseRegionHandler = Box<dyn FnMut(&TuiMouseEvent) -> Option<TuiMouseEventResult>>;

/// 鼠标区域（TS `class MouseRegion`）。
///
/// 渲染与失效完全委托给 `child`；只有鼠标事件额外经过 `on_mouse` 兜底。构造时需要同时
/// 给出子组件与处理器，二者之后都不能替换。
pub struct MouseRegion {
    child: Box<dyn Component>,
    on_mouse: MouseRegionHandler,
}

impl MouseRegion {
    /// `constructor(child, onMouse)`。
    ///
    /// `on_mouse` 只在子组件未消费事件时调用；不接受 `Option`，不需要处理时请传一个
    /// 恒返回 `None` 的闭包。
    pub fn new(child: Box<dyn Component>, on_mouse: MouseRegionHandler) -> Self {
        MouseRegion { child, on_mouse }
    }

    /// 被包装的子组件（只读）。
    pub fn child(&self) -> &dyn Component {
        self.child.as_ref()
    }
}

impl Component for MouseRegion {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.child.render(width)
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        // 子组件先处理；未处理（或结果被抑制冒泡）才调用外部 handler。
        // 注意 `dispatch_mouse_event` 在子组件返回 `None` **或**返回一个不满足
        // `stops_propagation()` 的结果时都返回 `None`；后者表示子组件主动放弃消费，此时
        // 回落给外部 handler 是有意的冒泡行为。
        match dispatch_mouse_event(self.child.as_mut(), event) {
            Some(result) => Some(result),
            None => (self.on_mouse)(event),
        }
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {
        self.child.invalidate();
    }
}
