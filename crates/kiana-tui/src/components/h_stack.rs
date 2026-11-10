//! components/h-stack.ts。
//!
//! 水平栈：先以整宽测量每个子项的内在宽度，再按 `allocateStackSizes` 分配宽度、以分配宽度
//! 二次渲染，最后用 `compositeTuiLine` 按列合成。
//!
//! # 职责与调用方
//!
//! [`HStack`] 是 TS `HStack extends Stack` 的水平版本，内部只持有一个 [`Stack`]（布局条目 +
//! 容器子项），渲染算法在本文件。crate 根把它导出给扩展与下游（`kiana_tui::HStack`），
//! 备用屏也可以把它设为布局根（布局节点由 [`Component::layout_node`] 提供）。
//!
//! # 一次 render 的流程
//!
//! 1. 以 `width.max(1)` 为安全宽度，用 [`visible_stack_entries`] 过滤 `visible` 返回 false
//!    的条目（视口高度传 `MAX_SAFE_INTEGER`：HStack 自己不做纵向裁剪）。
//! 2. 测量：把每个子项按**整个宽度**渲染一次，取输出行的最大可见宽度（[`visible_width`]）
//!    作为固有宽度。
//! 3. 分配：[`allocate_stack_sizes`] 以安全宽度为可用空间，按 basis/grow/shrink/min/max
//!    分配每个子项的列数（gap 已从可用宽度中扣除）。
//! 4. 定稿：按分配宽度再渲染每个子项（宽度为 0 的子项直接跳过），取最高行数作为整体高度，
//!    再按 `align` 做交叉轴对齐（`Stretch` 与 `Start` 都是顶端对齐，不拉伸子项内容）。
//! 5. 合成：从左到右用 [`composite_tui_line`] 把每个子项的行叠到同一行缓冲的对应列。
//!
//! # 约束与注意
//!
//! - 一次 `render` 里每个子项最多渲染两次（测量 + 定稿）；子项的 `render` 若有副作用
//!   （推进动画、更新缓存）会执行两次。宽度为 0 的子项只在测量阶段渲染。
//! - 分配器返回向下取整的整数列数，列位置由 `child_width + gap` 自行累加；gap 在
//!   [`Stack`] 构造时已归一为非负整数。
//! - [`composite_tui_line`] 在基行是图片行时原样返回基行，因此同一行中先合成的图片行不会
//!   被后续子项覆盖。
//! - 鼠标沿用 `Container` 的按行路由，不做按列命中（见 `handle_mouse` 的注释）。

use crate::components::stack::{Stack, StackOptions};
pub use crate::components::stack::{
    StackChild, StackEntry, StackEntryOptions, allocate_stack_sizes, visible_stack_entries,
};
use crate::layout_node::StackLayoutEntry;
use crate::layout_node::{LayoutNode, LayoutViewport, StackAlign, StackLayoutType};
use crate::text::visible_width;
use crate::tui::{Component, TuiMouseEvent, TuiMouseEventResult, composite_tui_line};

/// 水平栈（TS `class HStack extends Stack`）。
///
/// 组合而不是继承：条目与子项都保存在内部的 [`Stack`] 里，本类型只实现水平渲染。
pub struct HStack {
    stack: Stack,
}

impl Default for HStack {
    fn default() -> Self {
        HStack::new(Vec::new(), StackOptions::default())
    }
}

impl HStack {
    /// 构造。
    ///
    /// `children` 按 [`StackChild`] 语义逐个入栈；`options` 里的 gap/align 归一后保存在
    /// [`Stack`] 中。
    pub fn new(children: Vec<StackChild>, options: StackOptions) -> Self {
        HStack {
            stack: Stack::new(StackLayoutType::HStack, children, options),
        }
    }

    /// 空栈。
    ///
    /// 等价于 `Default`：无子项、默认选项。
    pub fn empty() -> Self {
        HStack::default()
    }

    /// 底层 [`Stack`]。
    ///
    /// 布局条目与容器子项是同一份共享状态（见 `stack` 模块文档的表示差异）。
    pub fn stack(&self) -> &Stack {
        &self.stack
    }

    /// 可变的底层 [`Stack`]。
    ///
    /// 通过它增删子项即可；HStack 自身没有缓存需要失效。
    pub fn stack_mut(&mut self) -> &mut Stack {
        &mut self.stack
    }

    /// `addChild(component)`。
    ///
    /// 转发到 [`Stack::add_child`]：同时追加布局条目与容器子项，两者顺序保持一致。
    pub fn add_child(&mut self, component: Box<dyn Component>) {
        self.stack.add_child(component);
    }

    /// `addChild(component, options)`。
    ///
    /// 同上，附带 basis/grow/shrink/min/max/visible 布局选项。
    pub fn add_child_with_options(
        &mut self,
        component: Box<dyn Component>,
        options: StackEntryOptions,
    ) {
        self.stack.add_child_with_options(component, options);
    }

    /// 布局条目。
    ///
    /// 与容器子项一一对应；`visible` 回调等选项保存在条目里。
    pub fn entries(&self) -> &[StackLayoutEntry] {
        self.stack.entries()
    }
}

impl Component for HStack {
    fn render(&mut self, width: usize) -> Vec<String> {
        // 安全宽度：0 宽时仍给子项 1 列，避免出现宽度为 0 的渲染请求。
        let safe_width = width.max(1);
        let viewport = LayoutViewport {
            width: safe_width,
            height: crate::layout_node::MAX_SAFE_INTEGER as usize,
        };
        let entries = visible_stack_entries(self.stack.entries(), viewport);
        if entries.is_empty() {
            return Vec::new();
        }

        // 测量阶段：子项按整宽渲染，取最长可见行宽作为固有宽度。
        let intrinsic_widths: Vec<f64> = entries
            .iter()
            .map(|entry| {
                entry
                    .component
                    .borrow_mut()
                    .render(safe_width)
                    .iter()
                    .map(|line| visible_width(line) as f64)
                    .fold(0.0f64, f64::max)
            })
            .collect();
        // 分配阶段：available = 整宽，grow/shrink 在这里生效（VStack 用的是不分配的测量模式）。
        let widths = allocate_stack_sizes(
            &entries,
            &intrinsic_widths,
            Some(safe_width as f64),
            self.stack.gap(),
        );
        // 定稿阶段：按分配宽度重渲染；宽度为 0 的子项不再渲染，但仍占一个 gap 的步进。
        let rendered: Vec<Vec<String>> = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let child_width = widths[index] as usize;
                if child_width == 0 {
                    Vec::new()
                } else {
                    entry.component.borrow_mut().render(child_width)
                }
            })
            .collect();
        let height = rendered.iter().map(Vec::len).max().unwrap_or(0);
        let mut result = vec![String::new(); height];
        let mut x: isize = 0;
        for (index, lines) in rendered.iter().enumerate() {
            let child_width = widths[index] as usize;
            // 交叉轴对齐：Center/End 计算整数偏移；Stretch 与 Start 都不加偏移（不拉伸内容）。
            let offset = match self.stack.align() {
                StackAlign::Center => height.saturating_sub(lines.len()) / 2,
                StackAlign::End => height.saturating_sub(lines.len()),
                StackAlign::Start | StackAlign::Stretch => 0,
            };
            for (row, line) in lines.iter().enumerate() {
                let target = row + offset;
                if target >= result.len() {
                    continue;
                }
                // composite_tui_line 负责按 total_width 裁剪；基行是图片行时原样返回基行。
                result[target] =
                    composite_tui_line(&result[target], line, x, child_width, safe_width);
            }
            x += child_width as isize + self.stack.gap() as isize;
        }
        result
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        // TS 的 HStack 只覆写 render、没有覆写 handleMouse，鼠标由继承的 Container 按行路由
        // （同类说明见 v_stack.rs）：
        // `handle_container_mouse` 只按 y 区间选择子项，并把事件的 x/width 原样传给子项，
        // 因此 HStack 不做按列命中。备用屏的布局派发会跳过带 `layout_node` 的组件、
        // 改按布局盒子命中（见 alt_screen.rs 的 `dispatch_mouse_to_layout_inner`）。
        crate::components::stack::handle_container_mouse(&mut self.stack.children, event)
    }

    fn handles_input(&self) -> bool {
        // TS 的 Stack 没有 handleInput。
        false
    }

    fn invalidate(&mut self) {
        // 只递归子项；HStack 自身没有渲染缓存。
        for child in self.stack.children.iter_mut() {
            child.invalidate();
        }
    }

    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        // Stack 的子项是共享句柄代理（SharedHandle）；焦点/挂载遍历靠它们下钻到底层组件。
        Some(&self.stack.children)
    }

    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        Some(&mut self.stack.children)
    }

    fn layout_node(&self) -> Option<LayoutNode> {
        // 每次调用都重新构造布局节点（条目为快照），供备用屏布局器使用。
        Some(self.stack.layout_node())
    }
}
