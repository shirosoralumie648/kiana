//! components/scroll-view.ts。
//!
//! 单子项滚动容器：滚动状态、跟随末尾、过滚动策略、滚动条可见性/活跃状态，并向布局器暴露
//! [`ScrollLayoutState`]（滚动条几何由 `layout.rs` 计算与绘制）。
//!
//! ## 与 TS 的表示差异
//!
//! 1. **继承 `Container`**：Rust 无继承。这里用 `children: Vec<Box<dyn Component>>`（恰好一个
//!    `crate::components::stack::SharedHandle`）复刻容器语义（焦点遍历、`Container.handleMouse`），
//!    另存同一个底层组件的 [`SharedComponent`] 供 `[LAYOUT_NODE]()` 使用。共享句柄代理会把
//!    [`crate::tui::Component::for_each_container_child`] 透传给底层组件，因此焦点路径/挂载
//!    判定能穿过 `ScrollView` 到达任意深度的子项（与 TS 的 `Stack extends Container` 一致）。
//! 2. **定时器**：TS 的 `setTimeout` 在到时后隐藏瞬时滚动条并 `requestRenderCallback()`。Rust 侧
//!    用惰性截止时间（[`std::time::Instant`]）：任何一次查询 `isScrollbarVisible` 都会先结算过期，
//!    结算命中时通过 [`ScrollView::set_request_render`] 安装的回调补发一次重绘请求（对应 TS
//!    定时器回调里的 `requestRenderCallback?.()`）。到期本身仍要靠宿主的一次查询/渲染来驱动。
//! 3. **`requestRender` 回调**：TS 的 `updateLayout(contentHeight, viewportHeight, requestRender)`
//!    会把回调存进 `requestRenderCallback`，之后的滚动方法都靠它请求重绘。Rust 的
//!    [`ScrollLayoutState::update_layout`] 参数是调用期的 `&dyn Fn()`（骨架冻结的签名；
//!    布局器只有 `&dyn Fn()` 的临时借用，回调本身没有 `'static` 生命周期），**无法**安全
//!    存储（把借用延长到 `'static` 是悬垂调用）。因此滚动方法通过
//!    [`ScrollView::set_request_render`] 安装的 `Rc<dyn Fn()>` 请求重绘：宿主（alt-screen）
//!    必须在构造后安装一次；未安装时滚动只改状态、不主动请求重绘（下一次外部触发的渲染
//!    仍会画出正确状态）。这是本单元唯一无法在组件内复刻的 TS 回调语义。
//! 4. **构造错误**：构造失败返回 [`ScrollViewError`]（`Display` 输出是当前稳定契约，改动需
//!    同步测试）。
//!
//! ## 一次典型的执行流
//!
//! 1. 构造：[`ScrollView::new`]（接管子项所有权）或 [`ScrollView::from_shared`]（包装已共享的
//!    子项）校验 `axis` 并把子项包成共享句柄；此时内容高度与视口高度都是 0，滚动偏移也是 0。
//! 2. 布局：`layout.rs` 在布局滚动节点时调用 [`ScrollLayoutState::update_layout`] 写入内容高度
//!    与视口高度，再读回 [`ScrollLayoutState::scroll_top`] 平移子盒；滚动条的几何与绘制同样在
//!    布局器里完成（[`crate::layout::get_scrollbar_geometry`]），本文件只回答“可见吗、thumb
//!    是否展开、用什么样式”。
//! 3. 输入：备用屏通过 [`crate::layout_node::ScrollViewControl`]（或 [`ScrollView`] 的同名方法）
//!    调用 `scrollTo`/`scrollBy`/…；滚轮也是它在分发（`TuiAltScreen::route_wheel` 按
//!    [`ScrollLayoutState::overscroll`] 决定是否把剩余增量传给下一个滚动视图），不是本组件的
//!    `handle_mouse`。
//! 4. 定时器：滚动条自动隐藏没有独立线程，宿主在渲染调度里调用
//!    [`ScrollLayoutState::poll_timers`] 结算（`TuiAltScreen::render_decision` 与
//!    `render_now` 都这么做）；`settled` 为真时宿主必须画一帧。
//!
//! ## 注意事项
//!
//! - 全部状态藏在 `Rc<ScrollViewState>` 的 `Cell`/`RefCell` 里，本类型不是 `Send`/`Sync`，
//!   只能在创建它的线程上使用（`tui-core` 模块文档的同一约束）。
//! - `children` 必须恰好一项：[`ScrollView::child`] 与 [`Component::render`] 直接索引
//!   `children[0]`。`add_child`/`remove_child`/`clear` 一律返回错误，但
//!   [`Component::container_children_mut`] 按 trait 文档是要给焦点/鼠标遍历写 `focused` 标志的；
//!   调用方若借它 `clear`（或移除第 0 项），上述 `children[0]` 索引就会 panic；`push` 不会
//!   立刻 panic，但会打破“恰好一项”的不变量——`render`/`child` 只看第一个子项，额外子项
//!   仍会被焦点/鼠标遍历看到。
//! - `scrollTo`/`scrollBy` 的算术必须保持 f64：`scrollBy(1e300)` 依赖“先夹取、再相减”而不是
//!   整数加法（回归见 `crates/kiana-tui/tests/layout.rs` 的
//!   `scroll_by_handles_very_large_finite_deltas_without_overflow`）。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::components::stack::{handle_container_mouse, wrap_child};
use crate::components::text_component::StyleFn;
use crate::layout_node::{
    LayoutNode, Overscroll, ScrollLayoutNode, ScrollLayoutState, ScrollbarVisibility,
    SharedComponent,
};
use crate::tui::{Component, TuiMouseEvent, TuiMouseEventResult};

/// 滚动条策略的别名再导出（本组件的 TS `ScrollViewScrollbar` 名字）。
///
/// 实际类型是 [`crate::layout_node::ScrollbarVisibility`]，枚举值与布局器共用。
pub use crate::layout_node::ScrollbarVisibility as ScrollViewScrollbar;

/// 跟随模式（TS `follow?: "none" | "end"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScrollViewFollow {
    /// 不跟随：内容高度变化时只夹取当前偏移，不会自动贴底（TS `"none"`，缺省值）。
    #[default]
    None,
    /// 跟随末尾（TS `"end"`）：只要仍处于 follow-end 状态，内容增长后滚到新的末尾。
    End,
}

/// `ScrollView` 构造错误（TS 里是 `throw new Error(...)`）。
///
/// 构造路径只有 [`ScrollView::new`] / [`ScrollView::from_shared`] 会返回它
/// （[`ScrollViewError::UnsupportedAxis`]）；[`ScrollView::add_child`] /
/// [`ScrollView::remove_child`] / [`ScrollView::clear`] 也会返回本枚举的对应变体
/// （[`ScrollViewError::HasExactlyOneChild`]、[`ScrollViewError::ChildCannotBeRemoved`]、
/// [`ScrollViewError::ChildCannotBeCleared`]），用以维持“恰好一个子项”的不变量。
/// 每个变体的 `Display` 文案是当前稳定契约（见各变体的注释），改动需同步测试。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScrollViewError {
    /// `Unsupported ScrollView axis: ${axis}`
    UnsupportedAxis(String),
    /// `ScrollView has exactly one child`
    HasExactlyOneChild,
    /// `ScrollView child cannot be removed`
    ChildCannotBeRemoved,
    /// `ScrollView child cannot be cleared`
    ChildCannotBeCleared,
}

impl std::fmt::Display for ScrollViewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScrollViewError::UnsupportedAxis(axis) => {
                write!(f, "Unsupported ScrollView axis: {axis}")
            }
            ScrollViewError::HasExactlyOneChild => write!(f, "ScrollView has exactly one child"),
            ScrollViewError::ChildCannotBeRemoved => {
                write!(f, "ScrollView child cannot be removed")
            }
            ScrollViewError::ChildCannotBeCleared => {
                write!(f, "ScrollView child cannot be cleared")
            }
        }
    }
}

impl std::error::Error for ScrollViewError {}

/// 选项（TS `ScrollViewOptions`）。
///
/// `axis` 用字符串保留 TS 的运行期校验（只有 `None`/`"vertical"` 合法，其余值走
/// [`ScrollViewError::UnsupportedAxis`]）。
#[derive(Clone, Default)]
pub struct ScrollViewOptions {
    /// 滚动轴。`None` 与 `Some("vertical")` 都合法；其余字符串在构造时返回
    /// [`ScrollViewError::UnsupportedAxis`]（TS 只支持垂直轴）。
    pub axis: Option<String>,
    /// 跟随末尾策略；`None` 等价于 [`ScrollViewFollow::None`]。
    pub follow: Option<ScrollViewFollow>,
    /// 是否作为“主滚动视图”：布局器把 `primary` 的滚动视图记入
    /// `LayoutFrame::primary_scroll_view`（后遍历到的覆盖先前的；一个都没有时退回遍历到的
    /// 第一个），备用屏的搜索与分页/滚轮/`scrollToPrompt` 等滚动定位都读它；文本选择则按
    /// 指针/选区锚点所在的滚动视图定位（缺省 `false`）。
    pub primary: Option<bool>,
    /// 过滚动策略（`Chain` 把用不完的滚轮增量交给下一个滚动视图，`Contain` 就地截断）；
    /// 缺省用 [`Overscroll`] 的 `Default` 实现（`Chain`）。
    pub overscroll: Option<Overscroll>,
    /// 滚动条可见性策略；缺省用 [`ScrollbarVisibility`] 的 `Default` 实现（`Hidden`）。
    pub scrollbar: Option<ScrollViewScrollbar>,
    /// 默认 `\x1b[90m…\x1b[39m`。绘制轨道时逐格调用。
    pub scrollbar_track_style: Option<StyleFn>,
    /// 默认 `\x1b[37m…\x1b[39m`。绘制 thumb 时调用（active 时传入实心块 `█`，否则 `┃`）。
    pub scrollbar_thumb_style: Option<StyleFn>,
    /// 默认 1000。构造时先归一化为 `max(0, floor(ms))`（`NaN` 经 `f64::max` 归为 0），
    /// 排程时再由 `node_timeout_ms` 把 `< 1` 或超出 Node 上界（`2^31-1`）的值折成 1 ms。
    pub scrollbar_hide_delay_ms: Option<f64>,
}

impl std::fmt::Debug for ScrollViewOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScrollViewOptions")
            .field("axis", &self.axis)
            .field("follow", &self.follow)
            .field("primary", &self.primary)
            .field("overscroll", &self.overscroll)
            .field("scrollbar", &self.scrollbar)
            .field(
                "scrollbar_track_style",
                &self.scrollbar_track_style.as_ref().map(|_| "<fn>"),
            )
            .field(
                "scrollbar_thumb_style",
                &self.scrollbar_thumb_style.as_ref().map(|_| "<fn>"),
            )
            .field("scrollbar_hide_delay_ms", &self.scrollbar_hide_delay_ms)
            .finish()
    }
}

/// `scrollTo` 选项（TS `ScrollViewScrollToOptions`）。
#[derive(Debug, Clone, Copy, Default)]
pub struct ScrollViewScrollToOptions {
    /// 目标恰为内容末尾时也**不**重新启用 follow-end。
    pub disable_follow: Option<bool>,
}

/// Node `setTimeout` 的 delay 归一化。
///
/// 构造期已按 TS 做 `Math.max(0, Math.floor(ms ?? 1000))`；Node 对不落在
/// `[1, 2^31-1]` 的 delay（含 `NaN`、`Infinity`、过大的值）一律按 1 ms 排程，因此极端值
/// 不会像 `Duration::from_secs_f64` 那样 panic（TS 只是把它交给 `setTimeout`）。
fn node_timeout_ms(delay_ms: f64) -> u64 {
    if (1.0..=2_147_483_647.0).contains(&delay_ms) {
        delay_ms as u64
    } else {
        1
    }
}

/// 滚动状态（TS 的 `ScrollView` 自身实现 `ScrollLayoutState`）。
///
/// 布局器只持有 `Rc<dyn ScrollLayoutState>`，滚动方法在组件侧被调用；两者共享同一个
/// `Rc<ScrollViewState>`，因此状态全部用 `Cell`/`RefCell`。
pub struct ScrollViewState {
    follow_end: bool,
    primary: bool,
    overscroll: Overscroll,
    current_scrollbar: Cell<ScrollbarVisibility>,
    scrollbar_hide_delay_ms: f64,
    current_scroll_top: Cell<usize>,
    content_height: Cell<usize>,
    current_viewport_height: Cell<usize>,
    following_end: Cell<bool>,
    follow_suppressed_at_end: Cell<bool>,
    request_render: RefCell<Option<Rc<dyn Fn()>>>,
    transient_scrollbar_visible: Cell<bool>,
    scrollbar_active: Cell<bool>,
    scrollbar_hide_deadline: Cell<Option<Instant>>,
    scrollbar_track_style: StyleFn,
    scrollbar_thumb_style: StyleFn,
}

impl ScrollViewState {
    fn max_scroll_top(&self) -> usize {
        self.content_height
            .get()
            .saturating_sub(self.current_viewport_height.get())
    }

    fn request_render(&self) {
        let callback = self.request_render.borrow().clone();
        if let Some(callback) = callback {
            callback();
        }
    }

    // 只有“auto 且内容高于视口”才有瞬时滚动条可显示；其余情况（Always/Hidden，或 auto
    // 但内容已适配视口）不会点亮瞬时滚动条，也不排隐藏定时器。Always 的常驻滚动条由布局器
    // 按 scroll_top 绘制，内容高于视口时滚动仍会移动 thumb。
    fn mark_scrollbar_activity(&self) {
        if self.current_scrollbar.get() != ScrollbarVisibility::Auto
            || self.content_height.get() <= self.current_viewport_height.get()
        {
            return;
        }
        self.transient_scrollbar_visible.set(true);
        self.scrollbar_hide_deadline.set(None);
        // active（悬停轨道/拖动 thumb）期间不排隐藏定时器，滚动条保持展开；退出 active 的
        // 那次 set_scrollbar_active 会再调用本方法，从而重新开始计时。
        if self.scrollbar_active.get() {
            return;
        }
        let delay_ms = node_timeout_ms(self.scrollbar_hide_delay_ms);
        // 惰性截止时间：这里只记录“何时该隐藏”，真正结算发生在下一次
        // settle_transient_expiry（见模块文档第 2 条）。checked_add 溢出时退到
        // 2^31-1 ms，与 node_timeout_ms 的上界一致。
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(delay_ms))
            .unwrap_or_else(|| Instant::now() + Duration::from_millis(2_147_483_647));
        self.scrollbar_hide_deadline.set(Some(deadline));
    }

    fn hide_transient_scrollbar(&self) {
        self.transient_scrollbar_visible.set(false);
        self.scrollbar_hide_deadline.set(None);
    }

    /// `scrollTo(scrollTop, { disableFollow })`（TS `ScrollView.scrollTo` 的完整实现）。
    ///
    /// 逻辑放在状态上，是为了让布局器通过 [`ScrollViewControl`] 控制滚动视图
    /// （备用屏渲染器只能拿到 `Rc<dyn ScrollLayoutState>`）；[`ScrollView::scroll_to`]
    /// 只是本方法的薄包装。
    fn control_scroll_to(&self, scroll_top: f64, disable_follow: bool) {
        let current = self.current_scroll_top.get() as f64;
        // 非有限值（NaN/±∞）保持原位置；有限值按 Math.trunc 向零取整（负数会先截断
        // 再在下面被夹到 0）。
        let requested = if scroll_top.is_finite() {
            scroll_top.trunc()
        } else {
            current
        };
        // 全程 f64（与 TS 的 `Math.trunc`/`Math.min`/`Math.max` 一致），避免把巨大的请求值
        // 先转成整数再比较。
        let max_scroll_top = self.max_scroll_top() as f64;
        let next = requested.max(0.0).min(max_scroll_top);
        // disableFollow 只在目标恰好是末尾时生效：显式滚到末尾后不再重新进入 follow-end，
        // 否则后续内容增长会把视图又拉回底部。
        let next_follow_suppressed_at_end = disable_follow && next == max_scroll_top;
        let next_following_end =
            !next_follow_suppressed_at_end && self.follow_end && next == max_scroll_top;
        // 偏移与两个跟随标志都没变时直接返回：既不改状态，也不请求重绘，避免把
        // “原地滚动”变成一次多余的重绘请求。
        if next == current
            && next_following_end == self.following_end.get()
            && next_follow_suppressed_at_end == self.follow_suppressed_at_end.get()
        {
            return;
        }
        let moved = next != current;
        self.current_scroll_top.set(next as usize);
        self.following_end.set(next_following_end);
        self.follow_suppressed_at_end
            .set(next_follow_suppressed_at_end);
        if moved {
            self.mark_scrollbar_activity();
        }
        self.request_render();
    }

    /// `scrollBy(lines)`（TS `ScrollView.scrollBy` 的完整实现）。
    fn control_scroll_by(&self, lines: f64) -> f64 {
        let requested = if lines.is_finite() {
            lines.trunc()
        } else {
            0.0
        };
        // 0 增量直接返回 0：不点亮滚动条，也不请求重绘。
        if requested == 0.0 {
            return 0.0;
        }
        // 整段算术保持 f64，min/max 在双精度下完成，避免 `i64` 溢出
        // （`scrollBy(1e300)` 会夹到末尾并返回巨大的未消费 delta）。
        let max_scroll_top = self.max_scroll_top() as f64;
        // 跟随末尾时以 max_scroll_top 作起点（TS 同款）：从贴底状态向上滚时，增量从
        // “内容末尾”这个基准扣减。
        let start = if self.following_end.get() {
            max_scroll_top
        } else {
            self.current_scroll_top.get() as f64
        };
        let next = (start + requested).max(0.0).min(max_scroll_top);
        let moved = next - start;
        let was_following_end = self.following_end.get();
        self.current_scroll_top.set(next as usize);
        self.following_end
            .set(self.follow_end && next == max_scroll_top);
        self.follow_suppressed_at_end.set(false);
        if moved != 0.0 {
            self.mark_scrollbar_activity();
        }
        if moved != 0.0 || self.following_end.get() != was_following_end {
            self.request_render();
        }
        // requested - moved 是未被消费的剩余增量（可能为负）：到达边界时整个增量都未消费，
        // 跨越边界时只有能移动的部分被消费。调用方（备用屏的滚轮链）用它决定是否把剩余量
        // 交给下一个滚动视图（见 [`Overscroll`]）。
        requested - moved
    }

    /// `scrollToStart()`（TS `ScrollView.scrollToStart` 的完整实现）。
    fn control_scroll_to_start(&self) {
        let next_following =
            self.follow_end && self.content_height.get() <= self.current_viewport_height.get();
        let changed =
            self.current_scroll_top.get() != 0 || self.following_end.get() != next_following;
        self.current_scroll_top.set(0);
        self.following_end.set(next_following);
        self.follow_suppressed_at_end.set(false);
        if changed {
            self.mark_scrollbar_activity();
            self.request_render();
        }
    }

    /// `scrollToEnd()`（TS `ScrollView.scrollToEnd` 的完整实现）。
    fn control_scroll_to_end(&self) {
        let next = self.max_scroll_top();
        let changed =
            self.current_scroll_top.get() != next || self.following_end.get() != self.follow_end;
        self.current_scroll_top.set(next);
        self.following_end.set(self.follow_end);
        self.follow_suppressed_at_end.set(false);
        if changed {
            self.mark_scrollbar_activity();
            self.request_render();
        }
    }

    /// 结算惰性隐藏定时器；返回本次是否真的到期隐藏。
    ///
    /// TS 的 `setTimeout` 回调在隐藏瞬时滚动条后调用 `requestRenderCallback?.()`。
    /// 惰性实现把"到期"推迟到下一次查询：本方法只负责结算，重绘请求由
    /// [`ScrollView::is_scrollbar_visible`] 在宿主查询时补发（渲染/布局期走 trait 方法，
    /// 不能在渲染中途重入宿主回调）。
    fn settle_transient_expiry(&self) -> bool {
        let Some(deadline) = self.scrollbar_hide_deadline.get() else {
            return false;
        };
        if Instant::now() >= deadline {
            self.scrollbar_hide_deadline.set(None);
            self.transient_scrollbar_visible.set(false);
            true
        } else {
            false
        }
    }
}

impl ScrollLayoutState for ScrollViewState {
    fn scroll_top(&self) -> usize {
        self.current_scroll_top.get()
    }

    fn primary(&self) -> bool {
        self.primary
    }

    fn overscroll(&self) -> Overscroll {
        self.overscroll
    }

    fn viewport_height(&self) -> usize {
        self.current_viewport_height.get()
    }

    fn get_content_width(&self, width: usize) -> usize {
        if self.current_scrollbar.get() == ScrollbarVisibility::Always && width > 1 {
            width - 1
        } else {
            width
        }
    }

    fn update_layout(
        &self,
        content_height: usize,
        viewport_height: usize,
        // TS 在这里保存 `requestRenderCallback = requestRender`；Rust 的签名只有调用期借用，
        // 无法安全存储（见模块文档第 3 条），重绘回调改由 `ScrollView::set_request_render` 安装。
        _request_render: &dyn Fn(),
    ) {
        self.content_height.set(content_height);
        self.current_viewport_height.set(viewport_height);
        let max_scroll_top = self.max_scroll_top();
        // 夹取偏移：follow-end 状态直接钉到新的末尾，其它情况只保证不超过末尾（内容变短时
        // 被夹到新的末尾，内容变长时保持用户当前位置）。
        if self.following_end.get() {
            self.current_scroll_top.set(max_scroll_top);
        } else {
            self.current_scroll_top
                .set(self.current_scroll_top.get().min(max_scroll_top));
        }
        // 已经不在末尾（内容变长或用户上滚）时，disableFollow 留下的抑制标记失效；仍在末尾
        // 则保留，避免内容再次增长后被自动拉回底部。
        if self.current_scroll_top.get() < max_scroll_top {
            self.follow_suppressed_at_end.set(false);
        }
        // follow-end 的重新进入条件：配置了 follow、当前正好在末尾、且没有被 disableFollow
        // 抑制过。
        if self.follow_end
            && self.current_scroll_top.get() == max_scroll_top
            && !self.follow_suppressed_at_end.get()
        {
            self.following_end.set(true);
        }
        // 内容已经能全部放进视口：没有滚动条可显示，顺带清掉计时器。
        if self.content_height.get() <= self.current_viewport_height.get() {
            self.hide_transient_scrollbar();
        }
    }

    fn scrollbar(&self) -> ScrollbarVisibility {
        self.current_scrollbar.get()
    }

    fn scroll_view_control(&self) -> Option<&dyn crate::layout_node::ScrollViewControl> {
        Some(self)
    }

    fn is_scrollbar_visible(&self) -> bool {
        if self.current_scrollbar.get() == ScrollbarVisibility::Always {
            return self.current_viewport_height.get() > 0;
        }
        if self.current_scrollbar.get() != ScrollbarVisibility::Auto
            || self.content_height.get() <= self.current_viewport_height.get()
        {
            return false;
        }
        // 布局/绘制路径调用本方法（layout.rs 计算滚动条几何时）：这里只结算到期，不请求
        // 重绘——渲染中途重入宿主回调会多排一帧。宿主主动查询请用
        // [`ScrollView::is_scrollbar_visible`]，它会在结算命中时补发重绘请求。
        self.settle_transient_expiry();
        self.transient_scrollbar_visible.get()
    }

    fn is_scrollbar_active(&self) -> bool {
        self.scrollbar_active.get()
    }

    fn scrollbar_track_style(&self, text: &str) -> String {
        (self.scrollbar_track_style)(text)
    }

    fn scrollbar_thumb_style(&self, text: &str) -> String {
        (self.scrollbar_thumb_style)(text)
    }

    fn set_request_render(&self, callback: Option<Rc<dyn Fn()>>) {
        *self.request_render.borrow_mut() = callback;
    }

    fn poll_timers(&self) -> crate::layout_node::ScrollViewTimers {
        let settled = self.settle_transient_expiry();
        crate::layout_node::ScrollViewTimers {
            settled,
            next_deadline: self.scrollbar_hide_deadline.get(),
        }
    }
}

impl crate::layout_node::ScrollViewControl for ScrollViewState {
    fn scroll_to(&self, scroll_top: f64, disable_follow: bool) {
        self.control_scroll_to(scroll_top, disable_follow);
    }

    fn scroll_by(&self, lines: f64) -> f64 {
        self.control_scroll_by(lines)
    }

    fn scroll_to_start(&self) {
        self.control_scroll_to_start();
    }

    fn scroll_to_end(&self) {
        self.control_scroll_to_end();
    }

    fn set_scrollbar_active(&self, active: bool) {
        if self.scrollbar_active.get() == active {
            return;
        }
        self.scrollbar_active.set(active);
        self.mark_scrollbar_activity();
        self.request_render();
    }

    fn follow_end(&self) -> bool {
        self.follow_end
    }

    fn is_following_end(&self) -> bool {
        self.following_end.get()
    }
}

/// 滚动视图（TS `class ScrollView extends Container`）。
pub struct ScrollView {
    /// 布局节点使用的组件句柄（与 `children[0]` 共享同一底层组件）。
    child: SharedComponent,
    /// 容器语义的子项列表（恰好一个）。
    children: Vec<Box<dyn Component>>,
    state: Rc<ScrollViewState>,
}

impl ScrollView {
    /// `constructor(component, options = {})`。
    ///
    /// 接管 `component` 的所有权，并把它包成共享句柄（[`ScrollView::child_shared`] 与布局
    /// 节点看到的是同一底层组件）。
    ///
    /// 错误：`options.axis` 不是 `None`/`"vertical"` 时返回
    /// [`ScrollViewError::UnsupportedAxis`]；其余选项都在这里归一化为默认值，不会再失败。
    pub fn new(
        component: Box<dyn Component>,
        options: ScrollViewOptions,
    ) -> Result<ScrollView, ScrollViewError> {
        let (child, handle) = wrap_child(component);
        ScrollView::build(child, vec![handle], options)
    }

    /// 从共享组件构造（调用方已持有 `Rc<RefCell<dyn Component>>`）。
    ///
    /// 与 [`ScrollView::new`] 的区别：不接管所有权——传入的是 `Rc` 克隆，调用方保留的句柄
    /// 仍指向同一底层组件，可以继续读写它。备用屏用它把隐式子文档包成主滚动视图
    /// （`follow: End`、`primary: true`）。错误条件与 `new` 相同。
    pub fn from_shared(
        component: SharedComponent,
        options: ScrollViewOptions,
    ) -> Result<ScrollView, ScrollViewError> {
        let (child, handle) = crate::components::stack::wrap_shared(component);
        ScrollView::build(child, vec![handle], options)
    }

    fn build(
        child: SharedComponent,
        children: Vec<Box<dyn Component>>,
        options: ScrollViewOptions,
    ) -> Result<ScrollView, ScrollViewError> {
        // 唯一的运行期校验（TS `axis` 只支持垂直）。其余选项在这里折成 TS 的缺省值；
        // `following_end` 的初值直接取 follow_end——第一次布局前还没有内容高度可判断。
        match options.axis.as_deref() {
            None | Some("vertical") => {}
            Some(axis) => return Err(ScrollViewError::UnsupportedAxis(axis.to_string())),
        }
        let follow_end = options.follow == Some(ScrollViewFollow::End);
        let default_track: StyleFn =
            std::sync::Arc::new(|text: &str| format!("\x1b[90m{text}\x1b[39m"));
        let default_thumb: StyleFn =
            std::sync::Arc::new(|text: &str| format!("\x1b[37m{text}\x1b[39m"));
        let state = ScrollViewState {
            follow_end,
            primary: options.primary.unwrap_or(false),
            overscroll: options.overscroll.unwrap_or_default(),
            current_scrollbar: Cell::new(options.scrollbar.unwrap_or_default()),
            scrollbar_hide_delay_ms: options
                .scrollbar_hide_delay_ms
                .unwrap_or(1000.0)
                .max(0.0)
                .floor(),
            current_scroll_top: Cell::new(0),
            content_height: Cell::new(0),
            current_viewport_height: Cell::new(0),
            following_end: Cell::new(follow_end),
            follow_suppressed_at_end: Cell::new(false),
            request_render: RefCell::new(None),
            transient_scrollbar_visible: Cell::new(false),
            scrollbar_active: Cell::new(false),
            scrollbar_hide_deadline: Cell::new(None),
            scrollbar_track_style: options.scrollbar_track_style.unwrap_or(default_track),
            scrollbar_thumb_style: options.scrollbar_thumb_style.unwrap_or(default_thumb),
        };
        Ok(ScrollView {
            child,
            children,
            state: Rc::new(state),
        })
    }

    /// 被滚动的子组件。
    ///
    /// 直接索引 `children[0]`：见模块文档“注意事项”里的单项不变量。
    pub fn child(&self) -> &dyn Component {
        self.children[0].as_ref()
    }

    /// 共享句柄（供布局/外部持有）。
    ///
    /// 返回的是 `Rc` 指针克隆，不复制组件；布局节点里的 `component` 与它是同一底层组件的
    /// 两个代理句柄（`children[0]` 与它共享 `inner`）。
    pub fn child_shared(&self) -> SharedComponent {
        Rc::clone(&self.child)
    }

    /// 滚动状态句柄。
    ///
    /// 与布局节点里保存的是同一个 `Rc<ScrollViewState>`：布局器/备用屏通过 trait 读状态、
    /// 调控制方法，看到的都是本组件的实时状态。
    pub fn state(&self) -> Rc<dyn ScrollLayoutState> {
        self.state.clone()
    }

    /// 安装/替换重绘回调（见模块文档第 3 条）。
    ///
    /// 回调在滚动/滚动条状态变化时被调用；实现先克隆 `Rc` 再调用，因此回调内部重入
    /// [`ScrollView::set_request_render`] 不会触发 `RefCell` 借用冲突。传 `None` 卸载。
    pub fn set_request_render(&self, callback: Option<Rc<dyn Fn()>>) {
        *self.state.request_render.borrow_mut() = callback;
    }

    /// `followEnd`（只读）。
    pub fn follow_end(&self) -> bool {
        self.state.follow_end
    }

    /// `isFollowingEnd`。
    ///
    /// 构造后初值等于 `follow_end`（此时还没测量内容）；布局/滚动之后反映实际状态。
    pub fn is_following_end(&self) -> bool {
        self.state.following_end.get()
    }

    /// `scrollTop`。
    pub fn scroll_top(&self) -> usize {
        self.state.current_scroll_top.get()
    }

    /// `viewportHeight`。
    pub fn viewport_height(&self) -> usize {
        self.state.current_viewport_height.get()
    }

    /// `scrollbar`。
    pub fn scrollbar(&self) -> ScrollViewScrollbar {
        self.state.current_scrollbar.get()
    }

    /// `isScrollbarVisible`。
    ///
    /// - `always`：视口高度 > 0（不看内容高度）。
    /// - `auto`：内容高于视口，且瞬时可见标志为真（滚动后会点亮，超时或内容适配后熄灭）。
    /// - `hidden`：恒为 false。
    ///
    /// 与 [`ScrollLayoutState::is_scrollbar_visible`] 的区别：本方法会先结算惰性隐藏定时器，
    /// 并在真正到期时补发一次重绘请求（对应 TS 定时器回调里的 `requestRender`）；trait 方法
    /// 供布局器在绘制期间调用，只结算不请求。
    pub fn is_scrollbar_visible(&self) -> bool {
        // TS 的隐藏定时器到期时会主动 `requestRender()`；惰性实现在结算到期时补发一次。
        if self.state.settle_transient_expiry() {
            self.state.request_render();
        }
        self.state.is_scrollbar_visible()
    }

    /// `isScrollbarActive`。
    pub fn is_scrollbar_active(&self) -> bool {
        self.state.is_scrollbar_active()
    }

    /// `setScrollbar(scrollbar)`。
    ///
    /// 值没变时直接返回。切到非 `auto` 会隐藏瞬时滚动条并清掉隐藏定时器；切回 `auto` 时，
    /// 仅当内容高于视口且滚轮/拖动正处于 active 才会重新点亮滚动条（active 期间仍不排隐藏
    /// 定时器）。`Always`/`Hidden` 或内容适配视口时没有瞬时滚动条，也不会有隐藏定时器。
    pub fn set_scrollbar(&self, scrollbar: ScrollViewScrollbar) {
        if scrollbar == self.state.current_scrollbar.get() {
            return;
        }
        self.state.current_scrollbar.set(scrollbar);
        if scrollbar != ScrollbarVisibility::Auto {
            self.state.hide_transient_scrollbar();
        } else if self.state.scrollbar_active.get() {
            self.state.mark_scrollbar_activity();
        }
        self.state.request_render();
    }

    /// `getContentWidth(width)`。
    ///
    /// 只有 `scrollbar` 为 `Always` 且 `width > 1` 时才留出最后一列给滚动条；`Auto` 不预留，
    /// 滚动条绘制在内容的最后一列上（覆盖式，见 `layout.rs` 的 `paint_scrollbar`）。
    pub fn get_content_width(&self, width: usize) -> usize {
        self.state.get_content_width(width)
    }

    /// `setScrollbarActive(active)`。
    ///
    /// 值没变时直接返回。active 表示滚动条处于活动状态（备用屏在悬停轨道时置 true）：期间
    /// thumb 画成实心块 `█`（由布局器读取），且不排隐藏定时器；结束 active 时，仅当 `auto`
    /// 且内容高于视口才重新开始计时（`Always`/`Hidden` 或内容适配视口时没有隐藏定时器）。
    pub fn set_scrollbar_active(&self, active: bool) {
        if active == self.state.scrollbar_active.get() {
            return;
        }
        self.state.scrollbar_active.set(active);
        self.state.mark_scrollbar_activity();
        self.state.request_render();
    }

    /// `scrollTo(scrollTop, options = {})`。
    ///
    /// 目标值先向零取整、再夹到 `[0, max_scroll_top]`；非有限值保持当前位置。
    /// `options.disable_follow == Some(true)` 且目标正好是末尾时，会置“末尾抑制”标记，
    /// 之后内容增长不会自动贴底（实现见私有方法 `ScrollViewState::control_scroll_to`）。
    pub fn scroll_to(&self, scroll_top: f64, options: ScrollViewScrollToOptions) {
        self.state
            .control_scroll_to(scroll_top, options.disable_follow == Some(true));
    }

    /// `scrollBy(lines)`：返回未被消费的 delta（全程按 JS number / f64 计算）。
    ///
    /// 正向增量在末尾、负向增量在顶部时，整个增量都未被消费；跨越边界时只有能移动的部分
    /// 被消费。返回值就是（带符号的）未消费量；备用屏的滚轮链用它决定是否把剩余量交给下一个
    /// 滚动视图。
    pub fn scroll_by(&self, lines: f64) -> f64 {
        self.state.control_scroll_by(lines)
    }

    /// `scrollToStart()`。
    ///
    /// 滚到顶部并退出 follow-end；如果配置了 follow 且内容本就不超过视口，则仍置
    /// `following_end = true`（此时顶部就是末尾）。
    pub fn scroll_to_start(&self) {
        self.state.control_scroll_to_start();
    }

    /// `scrollToEnd()`。
    ///
    /// 滚到当前末尾，并把 `following_end` 恢复成配置的 `follow_end`（因此配置了 follow 时
    /// 会重新进入贴底状态）。
    pub fn scroll_to_end(&self) {
        self.state.control_scroll_to_end();
    }

    /// `updateLayout(contentHeight, viewportHeight, requestRender)`。
    ///
    /// 布局器唯一的写入口：写入新的尺寸、夹取滚动偏移、维护 follow-end/抑制标记，内容不再
    /// 溢出视口时清掉瞬时滚动条。`request_render` 参数不会被保存（见模块文档第 3 条）；
    /// 宿主应改用 [`ScrollView::set_request_render`] 安装回调。
    pub fn update_layout(
        &self,
        content_height: usize,
        viewport_height: usize,
        request_render: &dyn Fn(),
    ) {
        self.state
            .update_layout(content_height, viewport_height, request_render);
    }

    /// `addChild`：TS 在这里抛错。
    ///
    /// Rust 侧总是返回 [`ScrollViewError::HasExactlyOneChild`] 且不改状态——`removeChild` /
    /// `clear` 同理，三者共同保证“恰好一个子项”的不变量（见模块文档“注意事项”）。
    pub fn add_child(&mut self, _component: Box<dyn Component>) -> Result<(), ScrollViewError> {
        Err(ScrollViewError::HasExactlyOneChild)
    }

    /// `removeChild`：TS 在这里抛错。
    pub fn remove_child(&mut self, _component: &dyn Component) -> Result<(), ScrollViewError> {
        Err(ScrollViewError::ChildCannotBeRemoved)
    }

    /// `clear`：TS 在这里抛错。
    pub fn clear(&mut self) -> Result<(), ScrollViewError> {
        Err(ScrollViewError::ChildCannotBeCleared)
    }
}

impl Component for ScrollView {
    fn render(&mut self, width: usize) -> Vec<String> {
        // 与 TS 相同：子项按“可用内容宽度”渲染；如果为滚动条留了 1 列，就在每行尾部补
        // 一个空格。注意这不是补齐到 width——只补 1 个空格，也不截断子项超宽的行。
        let content_width = self.get_content_width(width);
        let lines = self.children[0].render(content_width);
        if content_width == width {
            lines
        } else {
            lines.into_iter().map(|line| format!("{line} ")).collect()
        }
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        // 容器的鼠标语义：把事件交给唯一子项（`handle_container_mouse` 会先渲染子项求出高度、
        // 按 y 做命中并把坐标换算成子项局部坐标）。滚动视口本身的滚轮处理不在这里，由备用屏
        // 通过 [`crate::layout_node::ScrollViewControl`] 完成（见模块文档“执行流”第 3 步）。
        handle_container_mouse(&mut self.children, event)
    }

    fn handles_input(&self) -> bool {
        // TS 的 `Container` 没有 `handleInput`。
        false
    }

    fn invalidate(&mut self) {
        // 本组件没有自己的渲染缓存，转发给子项即可。
        for child in self.children.iter_mut() {
            child.invalidate();
        }
    }

    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        // 复刻 TS `ScrollView extends Container`：焦点/挂载遍历能把本组件当作容器下钻。
        Some(&self.children)
    }

    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        Some(&mut self.children)
    }

    fn layout_node(&self) -> Option<LayoutNode> {
        // 每次调用都新建节点（布局节点是即时视图，不是缓存）；`component` 与 `state` 都是
        // 共享句柄，布局器据此识别滚动节点并实时读写滚动状态。
        let state: Rc<dyn ScrollLayoutState> = self.state.clone();
        Some(LayoutNode::Scroll(ScrollLayoutNode {
            component: Rc::clone(&self.child),
            state,
        }))
    }
}
