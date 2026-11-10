//! components/v-stack.ts。
//!
//! 垂直栈：测量每个子项的自然高度（`allocateStackSizes` 的"测量模式"），按 `gap` 拼接；
//! 不裁剪总高度（裁剪发生在 `layout.ts` 的视口布局或 `ScrollView` 里）。
//!
//! # 这个文件负责的边界
//! `VStack` 只回答“给定宽度，把若干子项按顺序堆成哪些行”：不换行、不裁剪、不滚动、不缓存。
//! 每个子项自己决定渲染几行，`gap` 只控制相邻子项之间的空行数。需要按视口高度分配或裁剪
//! 的场景（`grow`/`shrink`、滚动）由布局引擎或 `ScrollView` 负责。
//!
//! # 与 TS 的结构对应（继承 → 组合）
//! TS 里 `VStack extends Stack`；Rust 没有类继承，因此这里把
//! [`crate::components::stack::Stack`] 作为唯一字段包裹。条目与容器子项的同步、`add_child`
//! 的共享句柄包装、布局节点构造全部复用 `Stack` 的实现（见 [`crate::components::stack`]
//! 的模块文档），本文件只做类型选择（`StackLayoutType::VStack`）与转发。
//!
//! 文件头的 `pub use` 对应 TS `v-stack.ts` 末尾从 `stack.ts` 再导出条目类型的写法
//! （`export type { StackChild, StackEntry, StackEntryOptions, StackOptions } from
//! "./stack.ts"`），因此这些名字既能从 `kiana_tui::components::v_stack::…` 使用，也会经
//! crate 根的 `index.rs` 再导出。`allocate_stack_sizes`/`visible_stack_entries` 在这里一并
//! 再导出是 Rust 侧的便利（TS 中它们定义在 `stack.ts`）。
//!
//! # 两条渲染路径
//! 1. **直接渲染**：父组件调用 [`crate::tui::Component::render`]，转到 crate 私有的
//!    `Stack::render_vertical`：以 `max(1, width)` 渲染每个可见子项，用
//!    `allocate_stack_sizes(..., None, gap)` 取自然高度后按 `gap` 拼接。
//! 2. **布局引擎**：`TuiAltScreen` 通过 [`crate::tui::Component::layout_node`] 取得 Stack
//!    节点，由 `layout.rs` 直接按条目布局子项，不会把 `render` 的输出当作该栈自身的行，
//!    `grow`/`shrink` 只在这条路径上生效。但测量阶段仍可能调用 `render`：没有 `basis` 的
//!    子项由 `measure_height` 实测行数（嵌套的 `VStack` 也走这条测量路径），`ScrollView`
//!    还需要内容组件整段渲染出的 `scroll_content_lines`。
//!
//! # 关键约束
//! - 直接渲染没有高度参数：传给 `visible` 回调的视口高度取 `MAX_SAFE_INTEGER`（表示不限高）；
//!   输出行数 = 各可见子项分配高度之和 + `gap * (可见子项数 - 1)`（没有可见子项时为 0 行）。
//! - 测量模式下 `allocate_stack_sizes` 既不生长也不收缩（`available_size = None`），只按
//!   `basis`/`min_size`/`max_size` 夹取自然高度，与 `spec/tui-components.md` §2.2 的 TS
//!   流程一致。
//! - `render` 不把行裁到 `width`，也不把行数裁到某个高度；每行宽度由子项自己保证
//!   （`Text`/`TruncatedText` 会补到整宽）。
//! - 鼠标命中按子项渲染行数累加，**不跳过 `gap` 行**（TS 继承的 `Container.handleMouse`
//!   同样不知道 `gap`）；具体命中算法见 `components::stack::handle_container_mouse` 的文档
//!   注释（该函数是 `pub(crate)`，公开文档里只能写成行内代码）。
//!
//! # 主要使用方
//! - `kiana-interactive` 的 `ChatViewport`：`root` 与 `dock` 都是 `VStack`，交给备用屏布局。
//! - `kiana-tui` 的集成测试与 bench（`tests/layout.rs`、`tests/tui_alt_screen.rs`、
//!   `benches/render_churn_bench.rs`）用它组装根布局。

use crate::components::stack::Stack;
pub use crate::components::stack::{
    StackChild, StackEntry, StackEntryOptions, StackOptions, allocate_stack_sizes,
    visible_stack_entries,
};
use crate::layout_node::StackLayoutEntry;
use crate::layout_node::{LayoutNode, StackLayoutType};
use crate::tui::{Component, TuiMouseEvent, TuiMouseEventResult};

/// 垂直栈（TS `class VStack extends Stack`）。
///
/// 组合而非继承：唯一字段是一个以 `StackLayoutType::VStack` 构造的
/// [`crate::components::stack::Stack`]。条目顺序同时就是垂直排列顺序与子项遍历顺序。
pub struct VStack {
    stack: Stack,
}

impl Default for VStack {
    /// 空栈：`gap = 0`、`align = Stretch`（`StackOptions::default()` 的两个字段都是 `None`，
    /// 由 `Stack::new` 归一化为上述默认值）。
    fn default() -> Self {
        VStack::new(Vec::new(), StackOptions::default())
    }
}

impl VStack {
    /// 构造（`children` 按 TS 的 `StackChild[]` 语义逐个入栈）。
    ///
    /// `children` 的顺序即垂直排列顺序；`options.gap` 未给时为 0、`options.align` 未给时为
    /// `StackAlign::Stretch`。每个子项都会被包成共享句柄，以便同时挂进布局条目与容器子项
    /// 列表，细节见 [`crate::components::stack`] 的模块文档。
    pub fn new(children: Vec<StackChild>, options: StackOptions) -> Self {
        VStack {
            stack: Stack::new(StackLayoutType::VStack, children, options),
        }
    }

    /// 空栈。
    ///
    /// 等价于 `VStack::default()`：没有子项，`gap = 0`、`align = Stretch`。
    pub fn empty() -> Self {
        VStack::default()
    }

    /// 底层 [`Stack`]（共享状态）。
    ///
    /// 这里看不到容器子项列表：`Stack::children` 是 crate 私有，外部读取子项请用
    /// [`crate::tui::Component::container_children`]。
    pub fn stack(&self) -> &Stack {
        &self.stack
    }

    /// 可变的底层 [`Stack`]。
    ///
    /// 用于调用 `VStack` 没有转发的操作（如 `add_shared`、`remove_child`、`clear`）；
    /// 这些操作由 `Stack` 负责同时维护 `entries` 与容器子项列表。
    pub fn stack_mut(&mut self) -> &mut Stack {
        &mut self.stack
    }

    /// `addChild(component)`。
    ///
    /// 默认选项各字段均为 `None`（`StackEntryOptions::default()`），条目里保存的也是 `None`。
    /// `grow = 0`、`shrink = 1`、`min_size = 0`、`max_size = MAX_SAFE_INTEGER` 由
    /// `allocate_stack_sizes` 在布局时按 TS 默认值套用；`Stack::push_entry` 只归一化给出的
    /// `Some` 值。
    pub fn add_child(&mut self, component: Box<dyn Component>) {
        self.stack.add_child(component);
    }

    /// `addChild(component, options)`。
    ///
    /// 给出的（`Some`）`grow`/`shrink`/`min_size`/`max_size` 在此刻由 `normalize_size` 归一化
    /// 后写入布局条目（非有限值回退各自的默认值，其余 floor 并夹到非负）；未给出的字段保持
    /// `None`，由 `allocate_stack_sizes` 在布局时套用 TS 默认值。`basis` 与 `visible` 原样
    /// 保存。条目没有后续修改选项的入口，因此选项必须在加入时给全。
    pub fn add_child_with_options(
        &mut self,
        component: Box<dyn Component>,
        options: StackEntryOptions,
    ) {
        self.stack.add_child_with_options(component, options);
    }

    /// 布局条目。
    ///
    /// 返回全部条目（包括 `visible` 回调会过滤掉的），顺序与子项一致；过滤发生在渲染或
    /// 布局时（`visible_stack_entries`）。
    pub fn entries(&self) -> &[StackLayoutEntry] {
        self.stack.entries()
    }
}

impl Component for VStack {
    fn render(&mut self, width: usize) -> Vec<String> {
        // 委托 crate 私有的 `Stack::render_vertical`：测量模式（grow/shrink 不参与）、不裁剪高度。
        self.stack.render_vertical(width)
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        // TS 的 `Stack` 只覆写 `render`；鼠标由继承的 `Container.handleMouse` 处理。
        // TS 的 Stack 从不调用 `Container.render`，`mouseLayout` 始终为空，因此每次现场按
        // `event.width` 渲染子项求高度、按顺序命中；Rust 的 `handle_container_mouse` 同样
        // 每次渲染（不像 `Container` 在宽度未变时会复用 `mouse_layout` 缓存）。
        // `Container.handleMouse` 不知道 `gap`：gap 行不会空出来，而会从下一个子项的
        // 第一行起参与命中；这里保持同样的行为。
        crate::components::stack::handle_container_mouse(&mut self.stack.children, event)
    }

    // TS 的 `Container`/`Stack` 没有 `handleInput`：返回 false 让键盘分发跳过本组件
    // （见 `Component::handles_input` 的文档）。
    fn handles_input(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {
        // 与 TS `Container.invalidate` 一致：递归所有子项（包括被 `visible` 过滤掉的），
        // 由子项自己决定清理哪些缓存。
        for child in self.stack.children.iter_mut() {
            child.invalidate();
        }
    }

    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        // 与 `entries` 平行的共享句柄代理（`Stack::children`），焦点/挂载遍历靠它穿过 VStack。
        // 更深的遍历由代理的 `for_each_container_child` 透传给底层组件，见 `Component` 的文档。
        Some(&self.stack.children)
    }

    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        Some(&mut self.stack.children)
    }

    fn layout_node(&self) -> Option<LayoutNode> {
        // `[LAYOUT_NODE]()`：把 `layout_type = VStack`、条目快照、`gap`、`align` 交给布局器。
        // `Stack::layout_node` 每次调用都重新克隆 `entries`，不是缓存对象。
        Some(self.stack.layout_node())
    }
}
