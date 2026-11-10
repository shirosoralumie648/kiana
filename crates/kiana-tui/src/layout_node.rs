//! layout-node.ts。
//!
//! 布局协议：组件通过 `layout_node()` 暴露一个**即时构造的视图**（不是缓存对象），
//! 布局器据此分配空间。`ScrollLayoutNode.state` 是 `ScrollView` 自身，因此布局过程中
//! 可以直接读写 `scrollTop`，并在布局完成后回写 `viewportHeight`。
//!
//! TS 的 `LAYOUT_NODE` 是全局注册符号（多个 kiana-tui 副本之间也能识别）；Rust 侧无法复刻
//! JS symbol，等价物是 [`LAYOUT_NODE`] 常量作为 [`LayoutComponent::layout_node`] 的标记，
//! 并保证"同一布局协议可被 extern 组件实现"。
//!
//! ## 与 TS 的表示差异
//!
//! TS 的 `LayoutNode` 里内嵌 `Component` 引用（同一对象、直接调用）。Rust 的 [`Component`]
//! 方法是 `&mut self`（`render` 需要可变借用），所以条目持有 [`SharedComponent`]
//! （`Rc<RefCell<dyn Component>>`）——布局器用 `borrow_mut()` 渲染，与 TS 的单线程语义一致。
//! 组件之间**共享**同一个 `Rc` 时，两处改动都可见（对应 TS 的引用语义）。
//!
//! ## 在系统中的位置
//!
//! 本模块定义布局协议的**数据契约**，并实现 Stack 的尺寸分配
//! （`clamp_size`、`distribute`、[`allocate_stack_sizes`]）；测量、定位与绘制在
//! `layout.rs`：
//!
//! - `layout.rs` 的 `layout_component` 通过 [`Component::layout_node`] 取得 [`LayoutNode`]，
//!   再按变体递归测量、分配与绘制；
//! - `components/stack.rs` 的 `Stack::layout_node` 构造 [`StackLayoutNode`]，
//!   `VStack`/`HStack` 的 `layout_node` 转发它；`Stack::render_vertical`（stack.rs）与
//!   `HStack::render`（h_stack.rs）直接调用 [`visible_stack_entries`] 与
//!   [`allocate_stack_sizes`] 生成渲染行，`v_stack.rs` 只做转发/再导出；
//! - `components/scroll_view.rs` 构造 [`ScrollLayoutNode`] 并实现 [`ScrollLayoutState`]；
//! - `alt_screen.rs` 通过 [`ScrollLayoutState`] 与 [`ScrollViewControl`] 驱动滚动、查询定时器。
//!
//! ## 节点是即时视图
//!
//! `layout_node()` 每次调用都重新构造节点；`Stack` 的实现还会克隆一份 `entries` 作为快照。
//! 布局器不缓存节点，因此组件修改条目、间距后，下一次布局自然生效；反过来，节点里的
//! `entries`/`gap`/`align` 是构造时的快照，不会回写 `Stack`。不过
//! [`StackLayoutEntry::component`] 与 [`ScrollLayoutNode::state`] 是 `Rc` 共享句柄，
//! 通过它们对底层组件或滚动状态的修改仍会实时可见。
//!
//! ## 一次 Stack 布局（`crate::layout` 的 Stack 分支）
//!
//! 1. [`visible_stack_entries`] 用 `visible` 回调过滤条目；回调收到的是**整帧**视口
//!    （`LayoutContext::viewport`），不是本 Stack 的分配尺寸。
//! 2. 没有 `basis` 的条目先渲染一次，测量固有尺寸。
//! 3. [`allocate_stack_sizes`] 按 basis/grow/shrink/min/max 算出每个条目的尺寸（整数语义的 f64）。
//! 4. 调用方把尺寸转成 `isize`/`usize` 落成矩形；`gap` 只计入相邻条目之间，不计首尾。
//!
//! ## 注意事项
//!
//! - [`SharedComponent`] 是 `Rc<RefCell<…>>`：同一组件在布局期间的 `borrow()`/`borrow_mut()`
//!   不能重叠；组件在自己的 `render` 里再次布局同一组件会触发 `RefCell` panic。
//! - [`allocate_stack_sizes`] 不保证各尺寸之和等于可用尺寸：没有可 grow/shrink 的候选时
//!   分配会提前结束，调用方只应把它当作“每个条目的分配值”。
//! - [`ScrollLayoutState`] 的多数方法有默认实现（供只做测量的测试替身使用），默认值不代表
//!   真实滚动视图行为；`ScrollView` 的实现见 `components/scroll_view.rs`。

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use crate::tui::Component;

/// 布局协议的全局注册名（TS `Symbol.for("@earendil-works/kiana-tui/layout-node")`）。
pub const LAYOUT_NODE: &str = "@earendil-works/kiana-tui/layout-node";

/// 可在布局节点里共享的组件句柄。
pub type SharedComponent = Rc<RefCell<dyn Component>>;

/// 把 `Rc<RefCell<T>>` 转成 `SharedComponent`（隐式转换由 `CoerceUnsized` 完成，这里给个函数
/// 以免调用处写长类型）。
pub fn shared<T: Component + 'static>(component: Rc<RefCell<T>>) -> SharedComponent {
    component
}

/// 布局测量用的视口尺寸（TS `LayoutViewport`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct LayoutViewport {
    /// 视口宽度（终端列，不是字节数）。
    pub width: usize,
    /// 视口高度（终端行）；组件渲染时常用 `MAX_SAFE_INTEGER as usize` 表示“不限高”。
    pub height: usize,
}

/// 堆叠条目可见性回调（TS `visible?: (viewport) => boolean`）。
///
/// 返回 `false` 的条目不参与测量、分配与渲染，也不占 `gap`（见 [`visible_stack_entries`]
/// 与 `crate::layout` 的 Stack 分支）。`viewport` 由调用方决定：布局器传整帧视口，
/// `VStack`/`HStack` 的 `render` 传 `{ width: 安全宽度, height: MAX_SAFE_INTEGER }`。
pub type VisibleFn = Rc<dyn Fn(LayoutViewport) -> bool>;

/// 堆叠条目（TS `StackLayoutEntry`）。
///
/// `basis` 为 `None` 表示 `"auto"`（用测量出的固有尺寸）；`Some(n)` 是固定基准尺寸。
/// 其余字段为 `None` 时取 TS 默认值（`grow: 0`、`shrink: 1`、`minSize: 0`、
/// `maxSize: Number.MAX_SAFE_INTEGER`），见 `components::stack` 的 `allocate_stack_sizes`。
#[derive(Clone)]
pub struct StackLayoutEntry {
    /// 子组件句柄；布局器通过它读取布局节点并调用 `render`。
    pub component: SharedComponent,
    /// `basis`：`None` 表示 `"auto"`（用测量到的固有尺寸），`Some(n)` 用固定基准尺寸。
    pub basis: Option<f64>,
    /// `grow`：分配剩余空间时的权重；`None` 等价于 0（不增长）。
    ///
    /// 只在 `available_size` 为 `Some` 时生效；测量模式（`None`）下被忽略。
    pub grow: Option<f64>,
    /// `shrink`：空间不足时的收缩权重；`None` 等价于 1。
    ///
    /// 实际权重是 `shrink * max(1, 当前尺寸)`，见 [`allocate_stack_sizes`]。
    pub shrink: Option<f64>,
    /// 收缩下界；`None` 等价于 0。
    ///
    /// [`allocate_stack_sizes`] 计算初始尺寸时会先向下取整并夹到非负；但 `distribute`
    /// 的候选判定与 `capacity` 直接使用本字段的原始值，因此收缩结果可能带小数。
    pub min_size: Option<f64>,
    /// 增长上界；`None` 等价于 `MAX_SAFE_INTEGER`。
    ///
    /// 初始尺寸由 `clamp_size` 向下取整并夹到 `[min_size, max_size]`（上界小于下界时抬到
    /// 下界）；`distribute` 增长时以本字段的原始值作为上界，最终尺寸可能超过取整后的值。
    pub max_size: Option<f64>,
    /// 可见性回调；`None` 表示始终可见。见 [`VisibleFn`]。
    pub visible: Option<VisibleFn>,
}

impl std::fmt::Debug for StackLayoutEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StackLayoutEntry")
            .field("component", &"<component>")
            .field("basis", &self.basis)
            .field("grow", &self.grow)
            .field("shrink", &self.shrink)
            .field("min_size", &self.min_size)
            .field("max_size", &self.max_size)
            .field("visible", &self.visible.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

impl StackLayoutEntry {
    /// 只有组件的条目（其余字段走默认值）。
    pub fn new(component: SharedComponent) -> Self {
        StackLayoutEntry {
            component,
            basis: None,
            grow: None,
            shrink: None,
            min_size: None,
            max_size: None,
            visible: None,
        }
    }
}

/// 堆叠类型（TS `StackLayoutNode.type`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StackLayoutType {
    /// 纵向堆叠：子项沿 y 轴依次排列。
    VStack,
    /// 横向堆叠：子项沿 x 轴依次排列。
    HStack,
}

/// 交叉轴对齐（TS `StackLayoutNode.align`）。
///
/// 以 HStack 为例：主（x）轴由尺寸分配决定，交叉（y）轴按本值摆放子项——`Stretch` 拉满
/// 盒子高度，其余取自然高度后靠上/居中/靠下。本仓库当前只在 HStack 路径读取该字段
/// （`crate::layout` 的 HStack 分支与 `components/h_stack.rs` 的 `render`），VStack 分支未使用。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum StackAlign {
    /// 交叉轴拉满（默认）。
    #[default]
    Stretch,
    /// 交叉轴起点对齐。
    Start,
    /// 交叉轴居中（偏移量向下取整）。
    Center,
    /// 交叉轴终点对齐。
    End,
}

/// 堆叠布局节点（TS `StackLayoutNode`）。
#[derive(Debug, Clone)]
pub struct StackLayoutNode {
    /// 堆叠方向。
    pub layout_type: StackLayoutType,
    /// 条目快照；顺序同时决定尺寸分配顺序与摆放顺序。
    pub entries: Vec<StackLayoutEntry>,
    /// 相邻条目之间的间距（单位随方向为行或列）；不占首尾。
    pub gap: f64,
    /// 交叉轴对齐；当前只在 HStack 路径被读取（见 [`StackAlign`]）。
    pub align: StackAlign,
}

/// 过滚动行为（TS `ScrollLayoutState.overscroll`）。
///
/// `alt_screen.rs` 的 `route_wheel` 按“最深滚动视图优先”分发滚轮增量：遇到 `Contain`
/// 立即停止尝试链上的下一个视图，`Chain` 则允许剩余量继续传递。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Overscroll {
    /// 到达边界后把剩余增量交给链上的下一个滚动视图（默认）。
    #[default]
    Chain,
    /// 到达边界即消费掉剩余增量，不继续链式传递。
    Contain,
}

/// 滚动条可见性策略（TS `ScrollViewScrollbar`）。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum ScrollbarVisibility {
    /// 从不显示滚动条，也不为它预留内容列（默认）。
    #[default]
    Hidden,
    /// 内容高于视口时按需显示；滚动活动期间短暂可见，随后由隐藏定时器收起。
    Auto,
    /// 始终显示（视口高度大于 0 时），并在 `get_content_width` 中为它预留 1 列。
    Always,
}

/// 滚动视图的**视口控制**面（TS `TuiAltScreen` 直接持有 `ScrollView` 并调用其公开方法）。
///
/// Rust 的布局只暴露 [`ScrollLayoutState`]（`Rc<dyn …>`），渲染器无法从状态句柄拿回具体的
/// `ScrollView` 组件。备用屏需要按布局盒子找到滚动视图并调用
/// `scrollTo`/`scrollBy`/`scrollToStart`/`scrollToEnd`/`setScrollbarActive`/`followEnd`，
/// 因此这里补一个可选控制面：`ScrollView` 的状态实现它，其它布局状态返回 `None`
/// （[`ScrollLayoutState::scroll_view_control`] 的默认实现）。
///
/// 与 `ScrollViewScrollToOptions` 的对应关系：`disable_follow` 就是
/// `{ disableFollow: true }`；`None`/`false` 与 TS 缺省一致。
pub trait ScrollViewControl {
    /// `scrollTo(scrollTop, { disableFollow })`。
    ///
    /// 实现先把 `scroll_top` 向零截断（非有限值保持当前值），再夹到 `[0, max_scroll_top]`；
    /// `disable_follow` 只在落点等于最大滚动位置时抑制 follow-end。
    fn scroll_to(&self, scroll_top: f64, disable_follow: bool);
    /// `scrollBy(lines)`：返回未被消费的 delta。
    ///
    /// 正数增加 `scrollTop`，负数反向；实现先向零截断并夹到边界。非有限值（`NaN`/`±∞`）
    /// 按 0 处理并返回 0，而不是把原值当作未消费量返回。处于 follow-end 时以
    /// `max_scroll_top` 为起点：已贴底时正增量不改变 `scrollTop`，而是全部未消费返回。
    fn scroll_by(&self, lines: f64) -> f64;
    /// `scrollToStart()`。
    fn scroll_to_start(&self);
    /// `scrollToEnd()`。
    fn scroll_to_end(&self);
    /// `setScrollbarActive(active)`。
    fn set_scrollbar_active(&self, active: bool);
    /// `followEnd`。
    ///
    /// 构造时的静态选项：是否允许自动跟随末尾。
    fn follow_end(&self) -> bool;
    /// `isFollowingEnd`。
    ///
    /// 当前是否处于跟随末尾状态（动态，随滚动位置变化）。
    fn is_following_end(&self) -> bool;
}

/// 滚动视图对布局器暴露的状态（TS `ScrollLayoutState`）。
///
/// 由 `ScrollView` 实现。`update_layout` 收 `&dyn Fn()` 作为 `requestRender` 回调
/// （TS 传函数；Rust 侧用 trait 对象，生命周期与调用期一致）。
///
/// 滚动条相关的 5 个方法（`scrollbar` / `is_scrollbar_visible` / `is_scrollbar_active` /
/// `scrollbar_track_style` / `scrollbar_thumb_style`）主要被 `layout.rs` 的滚动条绘制使用；
/// `ScrollView` 的同名公开查询方法（`components/scroll_view.rs` 的 `is_scrollbar_visible` /
/// `is_scrollbar_active`）也会转发到 trait 的实现。TS 里它们直接读 `ScrollView` 的属性，
/// 这里给出默认实现，便于只实现布局测量的测试替身。
pub trait ScrollLayoutState {
    /// 当前滚动偏移（行）。
    fn scroll_top(&self) -> usize;
    /// 是否是主滚动视图（`TuiAltScreen` 记住它用于搜索与"滚到顶/底"）。
    fn primary(&self) -> bool;
    /// 链式还是包含式过滚动。
    fn overscroll(&self) -> Overscroll;
    /// 最近一次 [`ScrollLayoutState::update_layout`] 来自布局器的视口高度。
    fn viewport_height(&self) -> usize;
    /// `getContentWidth(width)`：`scrollbar === "always" && width > 1` 时为滚动条预留 1 列。
    fn get_content_width(&self, width: usize) -> usize;
    /// `updateLayout(contentHeight, viewportHeight, requestRender)`。
    ///
    /// 布局器对每个 Scroll 节点在每次布局中调用一次。实现应保存视口高度、按新的内容高度
    /// 夹取 `scroll_top`（并处理 follow-end）；`request_render` 只在调用期间有效，实现可以
    /// 忽略它（`ScrollView` 就忽略，改由 [`ScrollLayoutState::set_request_render`] 安装回调）。
    fn update_layout(
        &self,
        content_height: usize,
        viewport_height: usize,
        request_render: &dyn Fn(),
    );

    /// `ScrollView.scrollbar`（`"hidden"`/`"auto"`/`"always"`）。
    fn scrollbar(&self) -> ScrollbarVisibility {
        ScrollbarVisibility::Hidden
    }

    /// `isScrollbarVisible`：`always` → `viewportHeight > 0`；
    /// `auto` → 内容高于视口且瞬时可见标志为真。
    fn is_scrollbar_visible(&self) -> bool {
        false
    }

    /// `isScrollbarActive`（拖动/悬停时 thumb 展开）。
    fn is_scrollbar_active(&self) -> bool {
        false
    }

    /// `scrollbarTrackStyle(text)`；默认恒等。
    fn scrollbar_track_style(&self, text: &str) -> String {
        text.to_string()
    }

    /// `scrollbarThumbStyle(text)`；默认恒等。
    fn scrollbar_thumb_style(&self, text: &str) -> String {
        text.to_string()
    }

    /// 视口控制面（见 [`ScrollViewControl`]）；非 `ScrollView` 的布局状态返回 `None`。
    fn scroll_view_control(&self) -> Option<&dyn ScrollViewControl> {
        None
    }

    /// 安装/替换重绘回调（Rust 专属：TS 在 `updateLayout` 里保存 `requestRender`）。
    ///
    /// 冻结的 [`ScrollLayoutState::update_layout`] 只拿到调用期的 `&dyn Fn()`，无法安全保存，
    /// 因此宿主（`TuiAltScreen`）在每次布局后通过本方法把 `'static` 回调装到布局树里的
    /// 每个滚动视图上（对应 TS 每次 `updateLayout` 都覆盖 `requestRenderCallback`）。
    fn set_request_render(&self, _callback: Option<Rc<dyn Fn()>>) {}

    /// 结算惰性定时器并给出下次到期时刻（TS 的 `setTimeout(... requestRender)`）。
    ///
    /// TS 里 `ScrollView` 的滚动条隐藏定时器到期后自己调用 `requestRenderCallback?.()`；
    /// Rust 侧没有执行器，冻结的 `update_layout` 又不能保存回调，因此改由宿主在渲染调度
    /// （`TuiAltScreen::render_decision`）里轮询：`settled` 表示定时器刚到期、状态已变化，
    /// 宿主应立即渲染一帧；`next_deadline` 用于安排下一次唤醒（`None` = 没有待触发的定时器）。
    fn poll_timers(&self) -> ScrollViewTimers {
        ScrollViewTimers::default()
    }
}

/// [`ScrollLayoutState::poll_timers`] 的返回值。
///
/// 宿主（`alt_screen.rs` 的 `render_decision`）只在 `TuiBase` 没有待处理重绘
/// （其决策为 `Idle`）时才轮询布局树里的滚动视图：`settled == true` 表示定时器刚到期、
/// 状态已变化，应立即渲染一帧。否则收集 `next_deadline`（还会与 flash、选择自动滚动的
/// 截止时刻合并）：已到期返回 `Now`，未到期返回 `Wait`；没有任何待触发定时器时返回
/// `Idle`。`Tui::render_now` 也会对布局树里的滚动视图调用 `poll_timers` 结算到期定时器。
#[derive(Clone, Copy, Debug, Default)]
pub struct ScrollViewTimers {
    /// 本次查询结算了已到期的定时器（滚动条可见性等状态已变化，需要渲染）。
    pub settled: bool,
    /// 下一个定时器到期时刻；`None` 表示没有待触发的定时器。
    pub next_deadline: Option<Instant>,
}

/// 滚动布局节点（TS `ScrollLayoutNode`）。
#[derive(Clone)]
pub struct ScrollLayoutNode {
    /// 被滚动的组件（TS `component`）。
    ///
    /// 布局器会按 `scroll_top` 平移它的盒子。
    pub component: SharedComponent,
    /// 滚动状态（TS `state`，就是 `ScrollView` 自身）。
    ///
    /// 布局器只通过这个 trait 读写滚动位置与视口，不依赖具体的 `ScrollView` 类型。
    pub state: Rc<dyn ScrollLayoutState>,
}

impl std::fmt::Debug for ScrollLayoutNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScrollLayoutNode")
            .field("component", &"<component>")
            .field("primary", &self.state.primary())
            .field("scroll_top", &self.state.scroll_top())
            .finish()
    }
}

/// 布局节点（TS `LayoutNode = StackLayoutNode | ScrollLayoutNode`）。
#[derive(Debug, Clone)]
pub enum LayoutNode {
    /// 堆叠容器（VStack/HStack）。
    Stack(StackLayoutNode),
    /// 滚动容器。
    Scroll(ScrollLayoutNode),
}

impl LayoutNode {
    /// TS 的 `node.type`（`"vstack"` / `"hstack"` / `"scroll"`）。
    pub fn type_name(&self) -> &'static str {
        match self {
            LayoutNode::Stack(stack) => match stack.layout_type {
                StackLayoutType::VStack => "vstack",
                StackLayoutType::HStack => "hstack",
            },
            LayoutNode::Scroll(_) => "scroll",
        }
    }
}

/// 布局组件（TS `LayoutComponent`）：实现了 [`Component`] 且提供布局节点。
///
/// 注意 TS 的 `[LAYOUT_NODE]` 是方法调用，返回**新构造**的节点。
///
/// 布局器实际读取的是 [`Component::layout_node`]（返回 `Option<LayoutNode>`），**不会**
/// 自动调用本 trait 的方法。因此实现了本 trait 的类型若要被布局，仍须让
/// [`Component::layout_node`] 返回 `Some(…)`——本 trait 不改变 `Component` 的默认 `None`。
pub trait LayoutComponent: Component {
    /// 返回当前布局节点；每次调用都重新构造。
    fn layout_node(&self) -> LayoutNode;
}

/// 取布局节点（TS `getLayoutNode`）。
///
/// TS 用鸭子判定（`typeof candidate[LAYOUT_NODE] === "function"`）；Rust 侧的等价判定是
/// [`Component::layout_node`]（默认 `None`）。
///
/// 返回 `None` 表示叶子组件：布局器改为调用 `render`；测量模式（`height` 为 `None`）下
/// 按输出的行数决定自然高度，有显式分配高度时直接使用该高度（见 `layout.rs` 的叶子分支）。
/// 返回的节点是调用时即时构造的，不含缓存语义。
pub fn get_layout_node(component: &dyn Component) -> Option<LayoutNode> {
    component.layout_node()
}

/// `Number.MAX_SAFE_INTEGER`。
///
/// 值为 `2^53 - 1`，在 `f64` 中可精确表示。这里有两个用途：作为 `max_size` 的缺省上限
/// （语义上“不限制”），以及 `VStack`/`HStack` 渲染时作为 `LayoutViewport.height` 的
/// “不限高”哨兵。代码里存在 `MAX_SAFE_INTEGER as usize` 的转换（见 `components/stack.rs`），
/// 可表示范围受 `usize` 宽度限制。
pub const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// `visibleStackEntries(entries, viewport)`：过滤掉 `visible` 返回 false 的条目。
///
/// TS 定义在 `components/stack.ts`，但 `layout.ts` 与 `Stack.render` 都要用；放在协议模块里
/// 保证单实现（`components/stack.rs` 再导出即可）。
///
/// - `visible` 为 `None` 的条目视为可见；回调用传入的 `viewport` 原样调用。
/// - 返回新 `Vec`，条目按原顺序克隆；`component` 是 `Rc` 克隆，不会复制底层组件。
/// - 调用方用返回的条目继续测量时，必须**基于过滤后的列表**生成 `intrinsic_sizes`，
///   否则下标与原始 `entries` 不再对应（`crate::layout` 就是这样做的）。
/// - 没有错误路径；回调 panic 会原样传播。
pub fn visible_stack_entries(
    entries: &[StackLayoutEntry],
    viewport: LayoutViewport,
) -> Vec<StackLayoutEntry> {
    entries
        .iter()
        .filter(|entry| {
            entry
                .visible
                .as_ref()
                .is_none_or(|visible| visible(viewport))
        })
        .cloned()
        .collect()
}

/// `Math.max(0, Math.floor(value))`；非有限值按 0 处理（JS 里 `Math.floor(NaN)` 是 NaN，
/// 随后的 `Math.max` 也会得到 NaN——调用方都先按 `?? fallback` 取过值，这里只处理数字）。
fn floor_max0(value: f64) -> f64 {
    if !value.is_finite() {
        0.0
    } else {
        value.floor().max(0.0)
    }
}

/// `clampSize(size, entry)`（`components/stack.ts`）。
///
/// `size`、`min_size`、`max_size` 都先 `floor` 并夹到非负（非有限值按 0）；若上界小于下界，
/// 把上界抬到下界，因此结果永远落在 `[min, max]` 且始终有限。
fn clamp_size(size: f64, entry: &StackLayoutEntry) -> f64 {
    let min = floor_max0(entry.min_size.unwrap_or(0.0));
    let max = floor_max0(entry.max_size.unwrap_or(MAX_SAFE_INTEGER)).max(min);
    min.max(max.min(floor_max0(size)))
}

/// `distribute(sizes, entries, amount, mode)`（`components/stack.ts`）。
///
/// 把 `amount`（正数）按权重分摊到 `sizes`，`grow` 为真时增加、为假时减少：
///
/// 1. 每轮先筛选候选：grow 要求 `grow > 0` 且未达 `max_size`；shrink 要求 `shrink > 0`
///    且高于 `min_size`。没有候选就直接结束（差额留在原地）。
/// 2. 权重：grow 用 `grow`；shrink 用 `shrink * max(1, 当前尺寸)`，即尺寸越大让得越多。
/// 3. 按条目顺序逐个分配 `min(remaining, max(1, floor(remaining * weight / totalWeight)),
///    capacity)`，其中 `capacity` 是到上界/下界的距离；`delta <= 0` 的条目跳过。
/// 4. 若一轮下来没有任何条目发生变化（`distributed == 0`），立即返回，避免死循环。
///
/// 注意 `capacity` 与候选判定直接使用条目里的原始 `min_size`/`max_size`（不经过
/// `clamp_size` 的取整）。`sizes` 的初始值来自 `clamp_size` 的取整；后续 `capacity`
/// 使用原始 `max_size`/`min_size`，因此最终值可越过取整后的初始上界。当这些字段带小数时，
/// 分摊结果也可能带小数——`allocate_stack_sizes` 的“整数”承诺只在选项路径
/// （`components/stack.rs` 会先向下取整）下严格成立。
fn distribute(sizes: &mut [f64], entries: &[StackLayoutEntry], amount: f64, grow: bool) {
    let mut remaining = amount;
    // 每轮重新计算候选与权重：上一轮可能把某些条目推到 min/max，下一轮它们就不再参与。
    while remaining > 0.0 {
        let candidates: Vec<usize> = (0..entries.len())
            .filter(|&index| {
                let entry = &entries[index];
                if grow {
                    entry.grow.unwrap_or(0.0) > 0.0
                        && sizes[index] < entry.max_size.unwrap_or(MAX_SAFE_INTEGER)
                } else {
                    entry.shrink.unwrap_or(1.0) > 0.0
                        && sizes[index] > entry.min_size.unwrap_or(0.0)
                }
            })
            .collect();
        if candidates.is_empty() {
            return;
        }
        // 权重只在“当前轮”有效：grow 是固定权重，shrink 还乘上当前尺寸，
        // 因此同一轮内先用快照，避免边分配边改变权重。
        let weights: Vec<f64> = candidates
            .iter()
            .map(|&index| {
                let entry = &entries[index];
                if grow {
                    entry.grow.unwrap_or(0.0)
                } else {
                    entry.shrink.unwrap_or(1.0) * sizes[index].max(1.0)
                }
            })
            .collect();
        let total_weight: f64 = weights.iter().sum();
        // 本轮实际分出去的总额；为 0 说明所有候选都被 capacity 卡住，继续循环没有意义。
        let mut distributed = 0.0f64;
        for (candidate_index, &index) in candidates.iter().enumerate() {
            if remaining <= 0.0 {
                break;
            }
            let weight = weights[candidate_index];
            let proposed = ((remaining * weight) / total_weight).floor().max(1.0);
            let capacity = if grow {
                entries[index].max_size.unwrap_or(MAX_SAFE_INTEGER) - sizes[index]
            } else {
                sizes[index] - entries[index].min_size.unwrap_or(0.0)
            };
            let delta = remaining.min(proposed).min(capacity);
            if delta <= 0.0 {
                continue;
            }
            sizes[index] += if grow { delta } else { -delta };
            remaining -= delta;
            distributed += delta;
        }
        if distributed == 0.0 {
            return;
        }
    }
}

/// `allocateStackSizes(entries, intrinsicSizes, availableSize, gap)`（`components/stack.ts`）。
///
/// 返回的每个值都是整数（与 TS 的 `Math.floor` 语义一致）。`available_size` 为 `None` 表示
/// “测量模式”（`grow`/`shrink` 全部忽略，只受 basis/min/max 夹取）。
///
/// ## 输入对齐
///
/// `entries` 与 `intrinsic_sizes` 按**下标**对应：缺少的固有尺寸按 0 处理，多余的忽略。
/// `basis` 为 `Some(n)` 时直接用 `n`，否则用 `intrinsic_sizes[index]`；两者都先经
/// `clamp_size` 向下取整并夹到 `[min_size, max_size]`。`gap` 只扣除 `(n - 1) * gap`
/// （`n <= 1` 时不扣），不占首尾。
///
/// ## 分配规则
///
/// 1. 基准尺寸之和小于 `content_size = max(0, floor(available_size) - (n-1)*gap)` 时，
///    把差额按 `grow` 权重分给还有增长空间的条目；
/// 2. 大于时按 `shrink` 权重收缩，直到 `min_size` 或差额用完；
/// 3. 相等时原样返回。
///
/// 返回值之和**不保证**等于 `content_size`：例如所有 `grow` 都为 0 时差额无法分配，
/// 收缩到 `min_size` 后也可能仍然超出行空间。
///
/// ## 边界
///
/// 纯计算，无 I/O、无 panic（所有索引都来自 `0..entries.len()`）。`NaN` 的 `basis`/
/// 固有尺寸在 `floor_max0` 中变成 0，再被夹到 `min_size`。“整数”承诺的例外见
/// `distribute` 的说明（min/max/gap 带小数时结果可能带小数）。
pub fn allocate_stack_sizes(
    entries: &[StackLayoutEntry],
    intrinsic_sizes: &[f64],
    available_size: Option<f64>,
    gap: f64,
) -> Vec<f64> {
    // 阶段 1：确定每个条目的基准尺寸并夹取。basis 优先于测量值；两者都没有时为 0。
    let mut sizes: Vec<f64> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let raw = match entry.basis {
                Some(basis) => basis,
                None => intrinsic_sizes.get(index).copied().unwrap_or(0.0),
            };
            clamp_size(raw, entry)
        })
        .collect();
    let Some(available_size) = available_size else {
        // 测量模式：调用方只想知道自然尺寸（VStack/HStack 的 render 就是这样用的）。
        return sizes;
    };
    // 阶段 2：可用尺寸先减掉条目之间的 gap，再与基准总和比较；不足 0 时按 0 处理。
    let content_size =
        (available_size.floor() - (entries.len() as f64 - 1.0).max(0.0) * gap).max(0.0);
    let total: f64 = sizes.iter().sum();
    if total < content_size {
        distribute(&mut sizes, entries, content_size - total, true);
    } else if total > content_size {
        distribute(&mut sizes, entries, total - content_size, false);
    }
    // 阶段 3：返回每个条目的分配值；调用方负责把它转成盒子尺寸。
    sizes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_node_type_names_match_ts() {
        let child: SharedComponent = Rc::new(RefCell::new(EmptyComponent));
        let node = LayoutNode::Stack(StackLayoutNode {
            layout_type: StackLayoutType::VStack,
            entries: vec![StackLayoutEntry::new(child.clone())],
            gap: 0.0,
            align: StackAlign::Stretch,
        });
        assert_eq!(node.type_name(), "vstack");
        let node = LayoutNode::Stack(StackLayoutNode {
            layout_type: StackLayoutType::HStack,
            entries: vec![StackLayoutEntry::new(child.clone())],
            gap: 0.0,
            align: StackAlign::Stretch,
        });
        assert_eq!(node.type_name(), "hstack");
        let node = LayoutNode::Scroll(ScrollLayoutNode {
            component: child,
            state: Rc::new(FakeScrollState),
        });
        assert_eq!(node.type_name(), "scroll");
    }

    #[test]
    fn get_layout_node_defaults_to_none() {
        let component = EmptyComponent;
        assert!(get_layout_node(&component).is_none());
    }

    #[test]
    fn entries_are_shared() {
        let child: SharedComponent = Rc::new(RefCell::new(EmptyComponent));
        let entry = StackLayoutEntry::new(child.clone());
        assert!(Rc::ptr_eq(&entry.component, &child));
    }

    struct EmptyComponent;

    impl Component for EmptyComponent {
        fn render(&mut self, _width: usize) -> Vec<String> {
            Vec::new()
        }
    }

    #[derive(Default)]
    struct FakeScrollState;

    impl ScrollLayoutState for FakeScrollState {
        fn scroll_top(&self) -> usize {
            0
        }
        fn primary(&self) -> bool {
            false
        }
        fn overscroll(&self) -> Overscroll {
            Overscroll::Chain
        }
        fn viewport_height(&self) -> usize {
            0
        }
        fn get_content_width(&self, width: usize) -> usize {
            width
        }
        fn update_layout(
            &self,
            _content_height: usize,
            _viewport_height: usize,
            _request_render: &dyn Fn(),
        ) {
        }
    }
}
