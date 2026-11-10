//! layout.ts：视口布局引擎（测量 → 分配 → 绘制 → 命中测试）。
//!
//! `renderLayoutFrame` 是备用屏（`TuiAltScreen`）与全屏视图的唯一布局入口：它把组件树
//! 摊平成带 `rect`/`clip` 的 [`LayoutBox`]，再把每一行画进固定高度的屏幕缓冲。
//!
//! ## 与 TS 的表示差异
//!
//! 1. **`LayoutBox.component` 是 [`SharedComponent`]（`Rc<RefCell<dyn Component>>`）**。
//!    TS 里 `Component` 是引用类型、`render()` 可直接调用；Rust 的 `Component::render` 需要
//!    `&mut self`，所以布局器通过 `Rc` 句柄渲染，盒子之间共享同一份组件（对应 TS 的引用语义）。
//! 2. **`LayoutBox.parent` 不保留**。TS 只写不读（`layout.ts:92`、`layout.ts:165` 是仅有的两处
//!    写入），任何消费方（`tui-alt-screen.ts`）都只读 `clip`/`component`/`rect`/`scrollContentLines`；
//!    Rust 里父指针要么是自引用 `Rc`（会泄漏），要么是索引（无人使用），因此省略。
//! 3. **坐标用 `isize`、尺寸用 `usize`**。滚动会把子盒子的 `y` 平移成负数（`y - scrollTop`），
//!    所以 `x`/`y` 必须带符号；`width`/`height` 在 TS 里已被 `Math.max(0, …)` 夹过，用无符号表示。
//!    所有 `Math.floor` 出现处都显式 `floor()`；百分比尺寸（`SizeValue::Percent`）则在
//!    [`crate::tui::SizeValue::resolve`] 里先 `floor()` 再返回。
//!
//! ## 渲染缓存
//!
//! `LayoutContext.renderCache` 按“组件身份 + 宽度”缓存 `render()` 结果。TS 用
//! `Map<Component, Map<number, string[]>>`（组件对象作键）；Rust 用组件的 `Rc` 指针地址作键
//! （同一帧内所有盒子都持有 `Rc` 克隆，地址不会复用）。
//!
//! ## 设计原因
//!
//! 这里采用**每帧全量重建**的模型：[`render_layout_frame`] 每次调用都新建 `LayoutContext`
//! 与空的 `render_cache`，从根组件重新测量、分配并绘制，没有跨帧的布局缓存。组件树在两次
//! 调用之间可以变化（测试 `rebuilds_geometry_after_content_changes` 就依赖第二次调用重新
//! 测量），所以本实现选择每次重新测量；只有同帧内的重复渲染（测量一次、绘制一次）才由
//! `render_cache` 去重。
//!
//! ## 一次 `render_layout_frame` 的执行流程
//!
//! 1. 尺寸归一化：宽、高各自 `floor` 后至少为 1（TS `Math.max(1, Math.floor(...))`）。
//! 2. 递归布局（`layout_component`）：
//!    - 叶子组件：调用 `render` 得到全部行，分配高度取显式高度或行数；
//!    - Stack：先用 `visible_stack_entries` 过滤条目，测量固有尺寸，再用
//!      `allocate_stack_sizes` 分配；VStack 沿 y 轴、HStack 沿 x 轴依次定位子盒；
//!    - Scroll：先按**旧** `scroll_top` 偏移布局内容，调用 `update_layout` 写回内容/视口高度，
//!      再用新旧 `scroll_top` 的差值平移子盒并重算 clip。
//! 3. 绘制（`paint_box`）：每个盒子先画自己的 `lines`，再递归子盒，最后画自己的滚动条。
//!    因此子盒覆盖父盒内容，滚动条覆盖该盒子子树的内容（但会被之后绘制的兄弟子树覆盖）。
//! 4. 返回 [`LayoutFrame`]，其 `lines` 已经是 `height` 行的合成结果；备用屏再在其上做
//!    光标解析、搜索高亮、overlay、选择与 flash 合成。
//!
//! ## 主要消费方
//!
//! - `alt_screen` 的 `TuiAltScreen::do_render`：唯一的常规布局入口，同时把 `request_render`
//!   传进来，供滚动状态在布局期间请求下一帧。
//! - 鼠标/滚轮/滚动条：`get_layout_boxes_at`（命中组件）、`get_scroll_views_at` 与
//!   `get_scroll_view_box`（定位滚动视图盒子）、[`get_scrollbar_geometry`] /
//!   [`get_scrollbar_geometry_with`]（滚动条列与 thumb 几何）。
//! - 搜索、文本选择、滚到 prompt：读取 [`LayoutFrame::primary_scroll_view`] 与
//!   [`LayoutBox::scroll_content_lines`] 的原始内容行。
//!
//! ## 注意事项
//!
//! - 单线程结构：`SharedComponent` 是 `Rc<RefCell<dyn Component>>`，布局期间会 `borrow()`
//!   取布局节点、`borrow_mut()` 调 `render()`。组件在自己的 `render` 里再次触发同一组件的
//!   布局渲染，会与已持有的可变借用冲突（`RefCell` 规则），应避免。
//! - 坐标单位是终端列/行，不是字节：文本宽度按 [`crate::text::visible_width`] 的规则
//!   （TAB 记 3 列，ANSI/OSC/APC 零宽，CJK/emoji 按字素宽度）。
//! - 滚动子盒的 `y` 可以是负数；绘制与命中都必须先用 `clip` 裁剪，不能直接用 `rect` 行号
//!   访问屏幕数组。
//! - 绘制阶段会剥掉行首的 OSC 133 区域标记，并按可见行数裁剪 Kitty 图片；两者都只改变
//!   写入屏幕的那一行，不改盒子里缓存的 `lines`/`scroll_content_lines`。

use std::collections::HashMap;
use std::rc::Rc;

use crate::layout_node::{
    LayoutNode, LayoutViewport, ScrollLayoutState, SharedComponent, StackAlign,
    allocate_stack_sizes, visible_stack_entries,
};
use crate::terminal_image::{
    crop_kitty_image_line, get_kitty_image_metadata, get_max_kitty_image_rows, is_image_line,
};
use crate::text::{
    extract_ansi_code, get_active_background_ansi, get_grapheme_cell_range,
    normalize_terminal_output, slice_by_column, visible_width,
};
#[cfg(test)]
use crate::tui::Component;
use crate::tui::{CURSOR_MARKER, composite_tui_line};

/// `OSC133_ZONE_PREFIX = /^(?:\x1b\]133;[ABC](?:\x07|\x1b\\))+/`：行首的一个或多个 OSC 133 区域标记。
///
/// 常量只保存标记前缀；区域字符（仅 A/B/C）与终止符（BEL 或 `ESC \`）由
/// `strip_osc133_zone_prefix` 逐字符校验，以免把 `D`/`P` 等其它标记误剥。
const OSC133_ZONE_PREFIX: &str = "\x1b]133;";

/// 屏幕上的矩形（TS `LayoutRect`），单位是终端列/行，不是字节或像素。
///
/// 坐标是**绝对屏幕坐标**：布局根从 `(0, 0)` 开始；滚动视图会把内容子盒按
/// `-scroll_top` 平移，所以 `y` 可以是负数。
///
/// `width`/`height` 用无符号表示，因为进入布局的尺寸都经过 `floor` 且非负。本模块生成的
/// 矩形里，零宽度只出现在 hstack 的零宽子项占位盒上（见 `layout_component`）；高度可以为
/// 0（空内容、显式 0 高度，或滚动视口为 0），此时盒子不会被绘制。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct LayoutRect {
    /// 左边界列（布局根为 0，hstack 子项依次右移；布局器不会产生负的 `x`）。
    pub x: isize,
    /// 上边界行；滚动内容平移后可为负。
    pub y: isize,
    /// 占用的终端列数。
    pub width: usize,
    /// 占用的终端行数。
    pub height: usize,
}

impl LayoutRect {
    /// 按字段顺序构造；不做取整、夹取或坐标变换，参数原样保存。
    ///
    /// 本模块大多数矩形经由本函数创建；`intersect` 是例外，它直接构造 `LayoutRect`，因为
    /// 右/下边界差可能为负，需要用 `usize::try_from(...).unwrap_or(0)` 夹到 0。调用方若要
    /// 传入浮点尺寸，需自己先 `floor` 并夹到非负（布局器内部写作 `height.floor().max(0.0) as usize`）。
    pub fn new(x: isize, y: isize, width: usize, height: usize) -> Self {
        LayoutRect {
            x,
            y,
            width,
            height,
        }
    }
}

/// 布局盒（TS `LayoutBox`）。
///
/// 布局盒是布局结果树的节点：`rect` 是屏幕上的位置与分配尺寸，`clip` 是它与所有祖先裁剪区
/// 求交后的可见区域（因此 `clip ⊆ rect`），`children` 的顺序就是绘制顺序。
///
/// `lines` 是“叶子”组件渲染出的全部行；`scroll_content_lines` 是滚动视图未裁剪的内容；
/// `scroll_view` 只出现在滚动节点上。
///
/// 盒子与组件不是一一对应：同一 `SharedComponent` 在组件树中出现几次就会有几个盒子，
/// 它们通过 `render_cache` 共用同一份、同宽度的 `render` 结果。
#[derive(Clone)]
pub struct LayoutBox {
    /// 该盒子对应的组件（TS `component`）。滚动盒对应 `ScrollView` 本身，子盒才是内容组件。
    pub component: SharedComponent,
    /// 相对屏幕的矩形（滚动内容可为负 `y`）。
    pub rect: LayoutRect,
    /// 可见裁剪区（父裁剪与 `rect` 的交集）。
    ///
    /// 始终满足 `clip ⊆ rect` 且宽高可能为 0；命中测试用 `clip`，所以被祖先裁剪掉的区域
    /// 不会命中。
    pub clip: LayoutRect,
    /// 子盒（顺序 == 绘制顺序）。
    pub children: Vec<LayoutBox>,
    /// 叶子组件的渲染结果（仅无布局节点的组件有）。
    ///
    /// 用 `Rc` 共享：滚动内容是叶子组件时，同一份渲染结果会同时作为子盒的 `lines` 与父
    /// 滚动盒的 `scroll_content_lines`（同一个 `Rc`，不复制行）。`None` 表示该盒子自身不
    /// 携带渲染行：Stack/Scroll 由子盒绘制；零宽占位盒与空容器则不绘制任何内容。
    pub lines: Option<Rc<Vec<String>>>,
    /// 绘制起点相对 `lines` 的偏移（`CURSOR_MARKER` 在可视区下方时上移）。
    ///
    /// 绘制时取 `source_index = line_offset + row - rect.y`。偏移大于 0 表示第一行不再对应
    /// `lines[0]`，而是从内容中部开始画，使 `CURSOR_MARKER` 所在行正好落在盒子最后一行；
    /// 这个补偿只在 `lines` 多于分配高度、分配高度大于 0 且光标行超出分配高度时发生。
    pub line_offset: usize,
    /// 该盒子承载的滚动视图状态（仅滚动节点有）。
    ///
    /// `paint_box` 用它绘制滚动条，`get_scroll_views_at` 用它做命中测试，
    /// `get_scroll_view_box` 用指针相等把它映射回盒子。
    pub scroll_view: Option<Rc<dyn ScrollLayoutState>>,
    /// 滚动视图的完整内容行（用于 Kitty 图片裁剪）。
    ///
    /// 这里是**未按视口裁剪**的内容；滚动内容为叶子组件时与子盒 `lines` 是同一个 `Rc`，
    /// 为 Stack/Scroll 时是另外缓存的整行渲染结果。备用屏的搜索、文本选择与滚到 prompt
    /// 直接读取它来定位原始行。
    pub scroll_content_lines: Option<Rc<Vec<String>>>,
    /// 图层（TS 里恒为 0；保留以复刻排序比较）。
    ///
    /// 布局器创建的所有盒子都写 0，所以 [`get_layout_boxes_at`] 的排序实际只剩深度降序；
    /// 字段本身保留以维持通用排序比较器；当前所有盒子都写 0。
    pub layer: u32,
}

impl std::fmt::Debug for LayoutBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayoutBox")
            .field("rect", &self.rect)
            .field("clip", &self.clip)
            .field("children", &self.children.len())
            .field("has_lines", &self.lines.is_some())
            .field("line_offset", &self.line_offset)
            .field("has_scroll_view", &self.scroll_view.is_some())
            .field("layer", &self.layer)
            .finish()
    }
}

/// 一帧布局结果（TS `LayoutFrame`）。
///
/// `lines` 已经是 `height` 行的合成结果（父盒内容 + 子盒 + 滚动条），备用屏可以继续在其上
/// 叠加光标、高亮与 overlay。`root` 保留整棵布局盒树，供鼠标命中、滚动条几何、搜索与选择
/// 复用——这些查询无法从 `lines` 完成，因为 `lines` 已经丢失了内容属于哪个盒子。
#[derive(Clone)]
pub struct LayoutFrame {
    /// 布局根盒；由 [`render_layout_frame`] 以 `(0, 0, width, height)` 和整屏 clip 布局。
    pub root: LayoutBox,
    /// 布局使用的终端列数（`floor` 后至少为 1）。
    pub width: usize,
    /// 布局使用的终端行数（`floor` 后至少为 1），也是 `lines` 的长度。
    pub height: usize,
    /// 长度为 `height` 的屏幕行（已合成），索引即屏幕行号；元素可含 ANSI/OSC/Kitty 序列。
    pub lines: Vec<String>,
    /// 主滚动视图。
    ///
    /// 遍历布局树时，每个 `primary()` 为真的滚动视图都会覆盖它，所以最终是**最后一个**
    /// primary；若没有任何 primary，则保留遍历到的第一个滚动视图。对应 TS 的
    /// `if (state.primary || !ctx.primaryScrollView) ctx.primaryScrollView = scrollView`。
    pub primary_scroll_view: Option<Rc<dyn ScrollLayoutState>>,
}

impl std::fmt::Debug for LayoutFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayoutFrame")
            .field("root", &self.root)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("lines", &self.lines)
            .field(
                "has_primary_scroll_view",
                &self.primary_scroll_view.is_some(),
            )
            .finish()
    }
}

/// 滚动条几何（TS `ScrollbarGeometry`）。
///
/// 由 [`get_scrollbar_geometry`] 根据滚动框的 `rect`/`clip` 与滚动状态算出。所有坐标都是
/// 绝对屏幕坐标；`track_top`/`thumb_top` 可以是负的（盒子被滚出屏幕上方），绘制方会再按
/// `clip` 与屏幕范围过滤。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct ScrollbarGeometry {
    /// 滚动条所在列（`rect` 的最后一列）；若该列不在 `clip` 的水平范围内，整个几何返回 `None`。
    pub column: isize,
    /// 轨道起始行（等于滚动盒的 `rect.y`）。
    pub track_top: isize,
    /// 轨道行数（等于滚动盒的 `rect.height`）。
    pub track_height: usize,
    /// thumb 起始行（`track_top + thumb_offset`）。
    pub thumb_top: isize,
    /// thumb 行数，至少为 1：按轨道高度与内容高度的比例计算，并夹在 `[min(2, track_height), track_height]`。
    pub thumb_height: usize,
    /// 最大滚动偏移（`content_height - track_height`，饱和到 0）；为 0 时 thumb 固定在顶部。
    ///
    /// `alt_screen` 用它把 thumb 的拖动位移反算成 `scroll_top`。
    pub max_scroll_top: usize,
}

/// 一次布局调用的帧级上下文：视口、同帧渲染缓存、重绘回调与主滚动视图。
struct LayoutContext<'a> {
    /// 整帧视口尺寸；Stack 的 `visible` 回调拿到的是它，而不是 Stack 自己的分配尺寸。
    viewport: LayoutViewport,
    /// 组件地址 -> (安全宽度 -> 渲染行)。见 `component_key` 与 `render_cached`。
    render_cache: HashMap<usize, HashMap<usize, Rc<Vec<String>>>>,
    /// 宿主传入的重绘请求，原样转交给滚动状态的 `update_layout`。
    request_render: &'a dyn Fn(),
    /// 遍历中记录的“主滚动视图”，规则见 [`LayoutFrame::primary_scroll_view`]。
    primary_scroll_view: Option<Rc<dyn ScrollLayoutState>>,
}

/// 组件身份键（`Rc` 指针地址）。
///
/// `Rc::as_ptr` 给出 `RefCell<dyn Component>` 的胖指针，转成 `*const ()` 后只保留数据地址
/// （丢弃 vtable）。缓存只活在一次 [`render_layout_frame`] 调用内，而布局树中的盒子都持有
/// `Rc` 克隆，所以被缓存的组件不会在帧内释放，地址可以安全地当作身份。
fn component_key(component: &SharedComponent) -> usize {
    Rc::as_ptr(component) as *const () as usize
}

/// `intersect(a, b)`：两个矩形的交集。
///
/// 起点取两者较大的 `x`/`y`，终点取两者较小的右/下边界；没有重叠时
/// `right - x` 或 `bottom - y` 为负，`usize::try_from` 失败后宽度/高度取 0
/// （对应 TS 的 `Math.max(0, ...)`）。
fn intersect(a: LayoutRect, b: LayoutRect) -> LayoutRect {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = (a.x + a.width as isize).min(b.x + b.width as isize);
    let bottom = (a.y + a.height as isize).min(b.y + b.height as isize);
    LayoutRect {
        x,
        y,
        width: usize::try_from(right - x).unwrap_or(0),
        height: usize::try_from(bottom - y).unwrap_or(0),
    }
}

/// `renderCached(context, component, width)`：按 (组件, 宽度) 缓存渲染结果。
///
/// - `width` 先 `floor` 再至少取 1，所以 0、负数或 NaN 宽度都会落到 1 列；
/// - 键是组件地址 + 该整数宽度；缓存只在一次 [`render_layout_frame`] 内有效；
/// - 组件的 `render` 通过 `borrow_mut()` 调用。同一组件同宽度的重复请求（例如 Stack 先测量
///   一个叶子条目、随后 `layout_component` 再要它的行，或滚动内容既作为子盒又作为
///   `scroll_content_lines`）只会渲染一次。
fn render_cached(
    context: &mut LayoutContext<'_>,
    component: &SharedComponent,
    width: f64,
) -> Rc<Vec<String>> {
    let safe_width = (width.floor() as usize).max(1);
    let key = component_key(component);
    if let Some(widths) = context.render_cache.get(&key)
        && let Some(lines) = widths.get(&safe_width)
    {
        return Rc::clone(lines);
    }
    let lines = Rc::new(component.borrow_mut().render(safe_width));
    context
        .render_cache
        .entry(key)
        .or_default()
        .insert(safe_width, Rc::clone(&lines));
    lines
}

/// `measureHeight`：组件在给定宽度下渲染出的行数。
///
/// 这会真实调用 `render`（并写缓存），因此测量形态必须与最终绘制宽度一致，否则缓存无法命中。
fn measure_height(
    context: &mut LayoutContext<'_>,
    component: &SharedComponent,
    width: f64,
) -> usize {
    render_cached(context, component, width).len()
}

/// `measureWidth`：组件在给定宽度下所有行的最大可见列宽（没有行时为 0）。
///
/// 用 [`crate::text::visible_width`] 而不是字节长度，保证 CJK/emoji/ANSI 行的宽度是终端列数。
fn measure_width(
    context: &mut LayoutContext<'_>,
    component: &SharedComponent,
    width: f64,
) -> usize {
    render_cached(context, component, width)
        .iter()
        .map(|line| visible_width(line))
        .max()
        .unwrap_or(0)
}

/// `translateBox(box, deltaY)`：把整棵子树的 `rect.y` 平移 `delta_y`。
///
/// 只改 `rect`，不碰 `clip`；调用方（滚动分支）随后必须用 `update_clips` 重算裁剪区，
/// 否则平移后的盒子仍带着旧位置的可见区域。
fn translate_box(target: &mut LayoutBox, delta_y: isize) {
    target.rect.y += delta_y;
    for child in &mut target.children {
        translate_box(child, delta_y);
    }
}

/// `updateClips(box, parentClip)`：自顶向下重算整棵子树的可见裁剪区。
///
/// `clip = intersect(parentClip, rect)`。布局时先按临时位置算过一遍，滚动分支平移子盒后
/// 会再调用本函数覆盖为最终位置对应的裁剪区。
fn update_clips(target: &mut LayoutBox, parent_clip: LayoutRect) {
    target.clip = intersect(parent_clip, target.rect);
    for child in &mut target.children {
        update_clips(child, target.clip);
    }
}

/// `layoutComponent(context, component, x, y, width, height, clip)`：递归布局一个组件。
///
/// - `width` 是分配宽度（`floor` 后至少 1 列）；`height` 为 `None` 表示测量模式（按内容
///   自然高度），`Some(h)` 表示固定高度；
/// - `x`/`y` 是盒子左上角的绝对屏幕坐标；滚动分支可能传入负 `y` 或先减去旧滚动偏移；
/// - `clip` 是**父级**已算好的裁剪区，返回盒子的 `clip` 由本函数与 `rect` 求交得到；
/// - 返回的盒子持有 `component` 的 `Rc` 克隆，布局结果因此在帧结束前与组件树同寿命。
///
/// 三个分支（叶子 / Scroll / Stack）的差异见函数体内的阶段注释。没有错误返回：组件
/// `render` 或布局节点实现里的 panic 会向上传播。
fn layout_component(
    context: &mut LayoutContext<'_>,
    component: &SharedComponent,
    x: isize,
    y: isize,
    width: f64,
    height: Option<f64>,
    clip: LayoutRect,
) -> LayoutBox {
    let safe_width = (width.floor() as usize).max(1);
    // 布局节点是即时视图（TS 每次调用都重新构造）；这里只借用一次 component 就释放，
    // 后续递归不再持有它的 RefCell 借用。
    let node = component.borrow().layout_node();

    let Some(node) = node else {
        // 叶子：没有布局节点的组件一次性渲染成所有行，盒子高度取分配高度或行数。
        let lines = render_cached(context, component, safe_width as f64);
        let allocated_height = match height {
            None => lines.len(),
            Some(height) => height.floor().max(0.0) as usize,
        };
        let mut line_offset = 0usize;
        // 分配高度不足以容纳所有行、且光标行落在可视区之外时，把绘制起点往后跳，
        // 使光标行显示在盒子最后一行。
        if lines.len() > allocated_height
            && allocated_height > 0
            && let Some(cursor_line) = lines.iter().position(|line| line.contains(CURSOR_MARKER))
            && cursor_line >= allocated_height
        {
            line_offset = cursor_line - allocated_height + 1;
        }
        let rect = LayoutRect::new(x, y, safe_width, allocated_height);
        return LayoutBox {
            component: Rc::clone(component),
            rect,
            clip: intersect(clip, rect),
            children: Vec::new(),
            lines: Some(lines),
            line_offset,
            scroll_view: None,
            scroll_content_lines: None,
            layer: 0,
        };
    };

    match node {
        LayoutNode::Scroll(node) => {
            // 顺序很重要：先用旧 scroll_top 布局内容，再写回 update_layout（它可能夹取
            // scroll_top 或按 follow-end 调整），最后用新旧差值把子盒平移到新位置。
            let previous_scroll_top = node.state.scroll_top();
            // scrollbar 为 always 时内容宽度会少 1 列，为滚动条预留；是否预留由滚动状态决定。
            let content_width = node.state.get_content_width(safe_width);
            let mut child_box = layout_component(
                context,
                &node.component,
                x,
                y - previous_scroll_top as isize,
                content_width as f64,
                None,
                clip,
            );
            // 内容高度取子盒布局后的高度，而不是重新渲染的行数：嵌套 Stack/Scroll 时
            // 子盒占用高度与其 `render` 行数可能不同（例如固定 basis 的条目）。
            let content_height = child_box.rect.height;
            let viewport_height = match height {
                None => content_height,
                Some(height) => height.floor().max(0.0) as usize,
            };
            // 布局器对滚动状态的唯一写入口。request_render 只按引用转交，是否保存/调用
            // 由具体实现决定（ScrollView 忽略它，改由宿主安装回调）。
            node.state
                .update_layout(content_height, viewport_height, context.request_render);
            // 旧值 - 新值：若 update_layout 把 scroll_top 往下夹（变小），子盒整体下移
            // 相应行数；反之向上移动。
            let delta = previous_scroll_top as isize - node.state.scroll_top() as isize;
            translate_box(&mut child_box, delta);
            let scroll_view = Rc::clone(&node.state);
            // 每个 primary 都会覆盖，所以最终记录的是最后一个 primary；没有任何 primary 时
            // 保留遍历到的第一个滚动视图。
            if scroll_view.primary() || context.primary_scroll_view.is_none() {
                context.primary_scroll_view = Some(Rc::clone(&scroll_view));
            }
            let rect = LayoutRect::new(x, y, safe_width, viewport_height);
            let child_clip = intersect(clip, rect);
            // 与子盒共用 render_cache：叶子内容的行不会二次渲染；若内容是 Stack，这里通过
            // 缓存取/生成 `VStack::render` / `HStack::render` 的结果（VStack 委托
            // `Stack::render_vertical`；按缓存键可能是首次或复用）。
            let scroll_content_lines =
                render_cached(context, &node.component, content_width as f64);
            let mut box_ = LayoutBox {
                component: Rc::clone(component),
                rect,
                clip: child_clip,
                children: vec![child_box],
                lines: None,
                line_offset: 0,
                scroll_view: Some(scroll_view),
                scroll_content_lines: Some(scroll_content_lines),
                layer: 0,
            };
            // 子盒可能刚被 translate_box 平移过，裁剪区必须按最终位置重算。
            update_clips(&mut box_.children[0], child_clip);
            box_
        }
        LayoutNode::Stack(node) => {
            // visible 回调拿到的是整帧视口，而不是本 Stack 的分配尺寸；
            // 被过滤掉的条目既不占尺寸也不产生 gap。
            let entries = visible_stack_entries(&node.entries, context.viewport);
            let gap = node.gap;
            let gap_total = (entries.len() as f64 - 1.0).max(0.0) * gap;

            if node.layout_type == crate::layout_node::StackLayoutType::VStack {
                // basis 优先；没有 basis 的条目按整宽测量固有高度。
                let intrinsic_heights: Vec<f64> = entries
                    .iter()
                    .map(|entry| match entry.basis {
                        Some(basis) => basis,
                        None => measure_height(context, &entry.component, safe_width as f64) as f64,
                    })
                    .collect();
                // height 为 None 时是测量模式：grow/shrink 被忽略，只按 basis/min/max 夹取；
                // Some(h) 时把剩余空间分配/收缩到各条目。
                let sizes = allocate_stack_sizes(&entries, &intrinsic_heights, height, gap);
                let natural_height: f64 = sizes.iter().sum::<f64>() + gap_total;
                // 仅在测量模式（height == None）下用自然高度；否则盒子固定为显式高度，
                // 子项分配已经按该高度做过 grow/shrink。
                let allocated_height = match height {
                    None => natural_height.max(0.0) as usize,
                    Some(height) => height.floor().max(0.0) as usize,
                };
                let rect = LayoutRect::new(x, y, safe_width, allocated_height);
                let mut box_ = LayoutBox {
                    component: Rc::clone(component),
                    rect,
                    clip: intersect(clip, rect),
                    children: Vec::new(),
                    lines: None,
                    line_offset: 0,
                    scroll_view: None,
                    scroll_content_lines: None,
                    layer: 0,
                };
                // 纵向依次摆放；每步前进“分配高度 + gap”。`gap` 字段是 f64，落到 `isize`
                // 坐标时通过 `as isize` 向零截断。
                let mut child_y = y;
                for (index, entry) in entries.iter().enumerate() {
                    let child = layout_component(
                        context,
                        &entry.component,
                        x,
                        child_y,
                        safe_width as f64,
                        Some(sizes[index]),
                        box_.clip,
                    );
                    box_.children.push(child);
                    child_y += sizes[index] as isize + gap as isize;
                }
                box_
            } else {
                // 第一阶段：按整宽测量每个条目的固有宽度（basis 优先），再横向分配。
                let intrinsic_widths: Vec<f64> = entries
                    .iter()
                    .map(|entry| match entry.basis {
                        Some(basis) => basis,
                        None => measure_width(context, &entry.component, safe_width as f64) as f64,
                    })
                    .collect();
                // 横向可用宽度固定为本盒宽度；gap 会先从可用空间中扣除，再分配剩余部分。
                let widths =
                    allocate_stack_sizes(&entries, &intrinsic_widths, Some(safe_width as f64), gap);
                // 第二阶段：按各自分配宽度（至少 1 列）测量高度，供交叉轴尺寸与对齐使用。
                let intrinsic_heights: Vec<usize> = entries
                    .iter()
                    .enumerate()
                    .map(|(index, entry)| {
                        measure_height(
                            context,
                            &entry.component,
                            (widths[index] as usize).max(1) as f64,
                        )
                    })
                    .collect();
                // 没有显式高度时，盒子高度取最高子项；有显式高度时直接采用。
                let allocated_height = match height {
                    None => intrinsic_heights.iter().copied().max().unwrap_or(0),
                    Some(height) => height.floor().max(0.0) as usize,
                };
                let rect = LayoutRect::new(x, y, safe_width, allocated_height);
                let mut box_ = LayoutBox {
                    component: Rc::clone(component),
                    rect,
                    clip: intersect(clip, rect),
                    children: Vec::new(),
                    lines: None,
                    line_offset: 0,
                    scroll_view: None,
                    scroll_content_lines: None,
                    layer: 0,
                };
                // 横向依次摆放；child_x 每步前进“分配宽度 + gap”。交叉轴对齐只影响 y：
                // stretch 拉满盒子高度，其余按 start/center/end 放置且不超过自然高度。
                let mut child_x = x;
                for (index, entry) in entries.iter().enumerate() {
                    let natural_child_height = intrinsic_heights[index];
                    let child_height = if node.align == StackAlign::Stretch {
                        allocated_height
                    } else {
                        allocated_height.min(natural_child_height)
                    };
                    let mut child_y = y;
                    match node.align {
                        StackAlign::Center => {
                            child_y +=
                                ((allocated_height as isize - child_height as isize) / 2).max(0);
                        }
                        StackAlign::End => {
                            child_y += allocated_height as isize - child_height as isize;
                        }
                        StackAlign::Stretch | StackAlign::Start => {}
                    }
                    let child_width = widths[index] as usize;
                    if child_width == 0 {
                        // 零宽子组件不 render（TS 直接构造盒子）。
                        box_.children.push(LayoutBox {
                            component: Rc::clone(&entry.component),
                            rect: LayoutRect::new(child_x, child_y, 0, child_height),
                            clip: LayoutRect::new(child_x, child_y, 0, 0),
                            children: Vec::new(),
                            lines: None,
                            line_offset: 0,
                            scroll_view: None,
                            scroll_content_lines: None,
                            layer: 0,
                        });
                    } else {
                        let child = layout_component(
                            context,
                            &entry.component,
                            child_x,
                            child_y,
                            child_width as f64,
                            Some(child_height as f64),
                            box_.clip,
                        );
                        box_.children.push(child);
                    }
                    child_x += child_width as isize + gap as isize;
                }
                box_
            }
        }
    }
}

/// `replaceScrollbarCell(line, column, totalWidth, replacement, preserveTargetBackground)`：
/// 用滚动条字形替换 `line` 中某一可见列，同时控制原内容 ANSI 状态的影响范围。
///
/// - 图片行不参与替换，原样返回（图片序列不能按列切分）；
/// - `column` 落在宽字符（CJK/emoji）内部时，整个字素区间都被替换，区间内多出来的列用
///   空格补齐，避免半个字符残留在屏幕上；
/// - 替换前输出 `\x1b[0m\x1b]8;;\x07`：重置 SGR 并关闭 OSC 8 链接，防止内容的前景色或
///   超链接漏到滚动条列；
/// - `preserve_target_background` 为真（滚动条策略不是 `always`）时，从被替换单元开头的
///   ANSI 序列里取出当时生效的背景色并重新加上，让覆盖式滚动条保留内容底色；`always`
///   策略下该列是滚动条专用列，连背景一起清掉；
/// - `total_width` 只用于计算右侧剩余列数，决定 `after` 片段取多长。
///
/// 返回新的行字符串，不修改输入。
fn replace_scrollbar_cell(
    line: &str,
    column: isize,
    total_width: usize,
    replacement: &str,
    preserve_target_background: bool,
) -> String {
    if is_image_line(line) {
        return line.to_string();
    }

    // 找到该列所属的字素区间；列落在宽字符中间时，区间会比 1 列宽。
    let column_u = usize::try_from(column).unwrap_or(usize::MAX);
    let grapheme_range = get_grapheme_cell_range(line, column_u);
    let start = grapheme_range.map_or(column_u, |range| range.start);
    let end = grapheme_range.map_or(column_u + 1, |range| range.end);
    let before = slice_by_column(line, 0, start, true);
    let target = slice_by_column(line, start, end.saturating_sub(start), true);
    let after = slice_by_column(line, end, total_width.saturating_sub(end), true);

    // 收集被替换单元开头的连续 ANSI，只用于提取背景色（前景由滚动条样式提供）。
    let mut target_prefix = String::new();
    let mut target_index = 0usize;
    while target_index < target.len() {
        let Some(ansi) = extract_ansi_code(&target, target_index) else {
            break;
        };
        target_prefix.push_str(&ansi.code);
        target_index += ansi.length;
    }
    // 左侧补齐处理宽字符跨到替换区间之前的部分；单元内补齐保证 replacement 正好占一列。
    let before_padding = " ".repeat(start.saturating_sub(visible_width(&before)));
    let cell_padding_before = " ".repeat(column_u.saturating_sub(start));
    let cell_padding_after = " ".repeat(end.saturating_sub(column_u).saturating_sub(1));
    let background = if preserve_target_background {
        get_active_background_ansi(&target_prefix)
    } else {
        String::new()
    };
    format!(
        "{before}{before_padding}\x1b[0m\x1b]8;;\x07{background}{cell_padding_before}{replacement}{cell_padding_after}{after}"
    )
}

/// `getScrollbarGeometry(box, includeHiddenAuto = false)`：不暴露“瞬时隐藏”的 `auto` 滚动条。
///
/// 等价于 [`get_scrollbar_geometry_with`]`(box, false)`；鼠标悬停命中需要包括隐藏轨道时
/// 用另一个函数。
pub fn get_scrollbar_geometry(box_: &LayoutBox) -> Option<ScrollbarGeometry> {
    get_scrollbar_geometry_with(box_, false)
}

/// `getScrollbarGeometry(box, includeHiddenAuto)`：计算滚动条列、轨道与 thumb 几何。
///
/// 返回 `None` 的情况（按检查顺序）：
/// 1. 盒子没有滚动视图，或 `rect.width == 0` / `rect.height == 0`；
/// 2. 滚动条不可见，且 `include_hidden_auto` 也没能放行——后者只额外允许
///    “`auto` 策略且内容高于视口”的瞬时隐藏滚动条，供鼠标悬停提前命中轨道
///    （`alt_screen.rs` 的 `update_scrollbar_hover`）；
/// 3. 滚动条列（`rect` 最后一列）不在 `clip` 的水平范围内时，整条滚动条不画。
///
/// `content_height` 优先取第一个子盒的 `rect.height`（受布局约束后的内容高度），其次取
/// `scroll_content_lines.len()`，都没有则为 0。thumb 高度按
/// `round(track_height² / content_height)` 计算并夹在 `[min(2, track_height), track_height]`；
/// `thumb_top` 按 `scroll_top / max_scroll_top` 的比例落在轨道内。
///
/// 不修改 `scroll_top`、不请求重绘；但 `is_scrollbar_visible()` 在 `ScrollView` 中会结算
/// 过期的瞬时隐藏，可能修改可见标志与隐藏定时器，因此不是严格无副作用。
pub fn get_scrollbar_geometry_with(
    box_: &LayoutBox,
    include_hidden_auto: bool,
) -> Option<ScrollbarGeometry> {
    let scroll_view = box_.scroll_view.as_ref()?;
    if box_.rect.width == 0 || box_.rect.height == 0 {
        return None;
    }

    let content_height = box_
        .children
        .first()
        .map(|child| child.rect.height)
        .or_else(|| box_.scroll_content_lines.as_ref().map(|lines| lines.len()))
        .unwrap_or(0);
    let track_height = box_.rect.height;
    let can_reveal_hidden_auto = include_hidden_auto
        && scroll_view.scrollbar() == crate::layout_node::ScrollbarVisibility::Auto
        && content_height > track_height;
    if !scroll_view.is_scrollbar_visible() && !can_reveal_hidden_auto {
        return None;
    }

    let min_thumb_height = 2usize.min(track_height);
    let proportional =
        js_round((track_height as f64 * track_height as f64) / content_height.max(1) as f64)
            as usize;
    let thumb_height = min_thumb_height.max(track_height.min(proportional));
    let max_scroll_top = content_height.saturating_sub(track_height);
    let max_thumb_top = track_height - thumb_height;
    let thumb_offset = if max_scroll_top == 0 {
        0
    } else {
        js_round((scroll_view.scroll_top() as f64 / max_scroll_top as f64) * max_thumb_top as f64)
            as usize
    };
    let column = box_.rect.x + box_.rect.width as isize - 1;
    if column < box_.clip.x || column >= box_.clip.x + box_.clip.width as isize {
        return None;
    }

    Some(ScrollbarGeometry {
        column,
        track_top: box_.rect.y,
        track_height,
        thumb_top: box_.rect.y + thumb_offset as isize,
        thumb_height,
        max_scroll_top,
    })
}

/// `Math.round`：正数上与 Rust 的 `f64::round` 相同（都是半值远离零）；这里只处理正数。
///
/// 本文件的调用点只有滚动条 thumb 高度/偏移，入参都是非负值，所以不必模拟 JS 对
/// 负数半值“向 +∞ 取整”的差异。
fn js_round(value: f64) -> f64 {
    value.round()
}

/// `paintScrollbar(box, screen, totalWidth)`：把滚动条字形画到已合成的屏幕行上。
///
/// 只处理 [`get_scrollbar_geometry`] 给出的可见几何；逐行跳过 `clip` 之外与屏幕之外的行。
/// thumb 在激活（拖动/悬停）时用实心块 `█`，否则用 `┃`；轨道用 `│`。每个格子都经过
/// `replace_scrollbar_cell`，以控制底层内容的前景/背景/超链接不泄漏。
///
/// 滚动条在 `paint_box` 的最后调用，所以覆盖同一盒子子树的内容，但会被之后绘制的兄弟
/// 子树覆盖。
fn paint_scrollbar(box_: &LayoutBox, screen: &mut [String], total_width: usize) {
    let Some(geometry) = get_scrollbar_geometry(box_) else {
        return;
    };
    let Some(scroll_view) = box_.scroll_view.as_ref() else {
        return;
    };
    let clip_bottom = box_.clip.y + box_.clip.height as isize;

    for offset in 0..geometry.track_height {
        let row = geometry.track_top + offset as isize;
        if row < box_.clip.y || row >= clip_bottom || row < 0 || row as usize >= screen.len() {
            continue;
        }
        let is_thumb =
            row >= geometry.thumb_top && row < geometry.thumb_top + geometry.thumb_height as isize;
        let replacement = if is_thumb {
            scroll_view.scrollbar_thumb_style(if scroll_view.is_scrollbar_active() {
                "\u{2588}"
            } else {
                "\u{2503}"
            })
        } else {
            scroll_view.scrollbar_track_style("\u{2502}")
        };
        let index = row as usize;
        screen[index] = replace_scrollbar_cell(
            &screen[index],
            geometry.column,
            total_width,
            &replacement,
            scroll_view.scrollbar() != crate::layout_node::ScrollbarVisibility::Always,
        );
    }
}

/// `sourceLine.replace(OSC133_ZONE_PREFIX, "")`：剥掉行首连续出现的 OSC 133 区域标记。
///
/// 每个标记形如 `ESC ] 133 ; <A|B|C> (BEL | ESC \)`，可以连续出现多个，循环全部剥掉。
/// 只处理**行首**；`D`/`P` 等其它区域标记不匹配、原样保留（测试
/// `strips_osc133_zone_prefixes` 明确锁定 `D` 不被剥离）。标记不完整时停在当前位置返回，
/// 不会 panic。
///
/// 返回借用自输入的切片；`paint_box` 会再 `to_string()` 才写入屏幕。
fn strip_osc133_zone_prefix(line: &str) -> &str {
    let mut rest = line;
    loop {
        let Some(after_prefix) = rest.strip_prefix(OSC133_ZONE_PREFIX) else {
            return rest;
        };
        // `[ABC]` 后跟 BEL 或 `ESC \`。
        let mut chars = after_prefix.char_indices();
        match chars.next() {
            Some((_, 'A' | 'B' | 'C')) => {}
            _ => return rest,
        }
        let mut terminator = None;
        if let Some((index, ch)) = chars.next() {
            if ch == '\x07' {
                terminator = Some(index + 1);
            } else if ch == '\x1b'
                && let Some((index2, '\\')) = chars.next()
            {
                terminator = Some(index2 + 1);
            }
        }
        match terminator {
            Some(end) => rest = &after_prefix[end..],
            None => return rest,
        }
    }
}

/// `paintBox(box, screen, totalWidth)`：把一棵布局盒子树合成进屏幕行。
///
/// 每个盒子的处理顺序：
/// 1. 若 `lines` 存在，按 `rect`/`clip`/`line_offset` 取可见行，先剥 OSC 133 前缀，
///    再按剩余可见行数裁剪 Kitty 图片，最后整行写入（全宽且目标行为空，或为图片行）
///    或按列合成；
/// 2. 递归所有子盒（绘制顺序 = `children` 顺序，后画的覆盖先画的）；
/// 3. 若盒子是滚动盒、`scroll_content_lines` 存在且 `scroll_top > 0`，在视口上方找最近的
///    一张 Kitty 图片，裁掉被滚出视口的行后画到视口首行（只有全宽且目标行在屏幕内时才
///    实际覆盖）；
/// 4. 最后画滚动条。
///
/// `screen` 的长度是帧高度，索引即屏幕行号；除滚动后的图片修补只检查屏幕范围外，其余
/// 行号访问都先与 `clip` 和 `screen.len()` 求交，因此负 `rect.y` 与滚出屏幕的盒子不会越界。
fn paint_box(box_: &LayoutBox, screen: &mut [String], total_width: usize) {
    if let Some(lines) = box_.lines.as_ref() {
        let offset = box_.line_offset;
        // 可见行窗口 = rect、clip 与屏幕三者的交集；rect.y 可能为负。
        let first_row = box_.rect.y.max(box_.clip.y).max(0);
        let last_row = (box_.rect.y + box_.rect.height as isize)
            .min(box_.clip.y + box_.clip.height as isize)
            .min(screen.len() as isize);
        for row in first_row..last_row {
            let source_index = offset as isize + row - box_.rect.y;
            if source_index < 0 {
                continue;
            }
            let Some(source_line) = lines.get(source_index as usize) else {
                continue;
            };
            let mut line = strip_osc133_zone_prefix(source_line).to_string();
            // 图片行会占多个终端行：只剩部分可见时用 crop_kitty_image_line 按可见行数
            // 重编码 y/h/r，只保留视口内的部分。
            if let Some(image_metadata) = get_kitty_image_metadata(&line) {
                let clip_bottom = screen
                    .len()
                    .min((box_.clip.y + box_.clip.height as isize) as usize);
                let visible_rows = image_metadata
                    .rows
                    .min(clip_bottom.saturating_sub(row as usize));
                if visible_rows < image_metadata.rows {
                    line = crop_kitty_image_line(&line, 0, visible_rows as isize);
                }
            }
            let index = row as usize;
            // 全宽快路径：整行宽盒子画到未触碰的行上时可直接用源行（合成会逐帧重建整行）。
            // 图片行也总走这里：图片序列不能安全地按可见列合成，必须整行写入。
            if box_.rect.x == 0
                && box_.rect.width >= total_width
                && (is_image_line(&line) || screen[index].is_empty())
            {
                screen[index] = line;
            } else {
                // 否则按列合成：保留基行左右两侧的内容，把 `line` 放到
                // `[rect.x, rect.x + rect.width)` 区间内。
                screen[index] = composite_tui_line(
                    &screen[index],
                    &line,
                    box_.rect.x,
                    box_.rect.width,
                    total_width,
                );
            }
        }
    }
    // 子盒后画，因此覆盖父盒的同一批屏幕行；兄弟之间按 `children` 顺序依次覆盖。
    for child in &box_.children {
        paint_box(child, screen, total_width);
    }

    // 滚动后视口顶部可能落在图片中间：内容子盒按平移后的行绘制，图片顶行在 clip 之上会
    // 被跳过，这里用裁剪后的图片覆盖视口首行，补上缺失的顶部（下边界的裁剪在逐行绘制时
    // 完成）。
    if let (Some(scroll_view), Some(scroll_content_lines)) = (
        box_.scroll_view.as_ref(),
        box_.scroll_content_lines.as_ref(),
    ) && scroll_view.scroll_top() > 0
        && box_.rect.height > 0
    {
        // 找视口上方最近的一张图片行；搜索范围以已注册的最高图片行数为界
        // （图片行下方的占位行可能含 padding/边框/并排文本）。
        let scroll_top = scroll_view.scroll_top();
        let min_row = scroll_top.saturating_sub(get_max_kitty_image_rows());
        let mut image_row = scroll_top;
        while image_row > min_row {
            image_row -= 1;
            let image_line = scroll_content_lines
                .get(image_row)
                .map(String::as_str)
                .unwrap_or("");
            if !is_image_line(image_line) {
                continue;
            }
            let Some(metadata) = get_kitty_image_metadata(image_line) else {
                continue;
            };
            let hidden_rows = scroll_top - image_row;
            if hidden_rows < metadata.rows {
                let visible_rows = box_.rect.height.min(metadata.rows - hidden_rows);
                let cropped =
                    crop_kitty_image_line(image_line, hidden_rows as isize, visible_rows as isize);
                let target = box_.rect.y;
                // 只有全宽且目标行在屏幕内时才覆盖；非全宽布局保持子盒已经画好的内容。
                if box_.rect.x == 0
                    && box_.rect.width >= total_width
                    && target >= 0
                    && (target as usize) < screen.len()
                {
                    screen[target as usize] = cropped;
                }
            }
            // 只处理最近的一张图片行（与 TS 的 break 行为一致），不再继续向上搜索。
            break;
        }
    }

    // 滚动条最后画，覆盖该盒子子树（含图片修正）的内容。
    paint_scrollbar(box_, screen, total_width);
}

/// `renderLayoutFrame(root, width, height, requestRender)`：布局并绘制一整帧。
///
/// 输入：根组件、以终端列/行为单位的宽高（`f64`，内部 `floor` 后至少为 1），以及一个重绘
/// 请求回调。回调被放进 `LayoutContext` 并原样传给每个滚动节点的
/// [`crate::layout_node::ScrollLayoutState::update_layout`]；是否保存或调用由滚动状态决定
/// （`ScrollView` 的实现当前忽略它，重绘回调由宿主另行安装）。
///
/// 副作用：布局滚动节点时调用 `update_layout`，可能夹取 `scroll_top` 或调整 follow-end
/// 状态；这些变化会反映在返回帧的盒子坐标里（见 `layout_component` 的滚动分支）。
/// 返回的 [`LayoutFrame::lines`] 已经是合成后的屏幕行。
///
/// 返回类型不是 `Result`：组件 `render` 或布局节点实现里的 panic 会原样向上传播。
/// 本函数在调用线程同步执行，自身不创建线程、不做 I/O。
pub fn render_layout_frame(
    root: SharedComponent,
    width: f64,
    height: f64,
    request_render: &dyn Fn(),
) -> LayoutFrame {
    let safe_width = (width.floor() as usize).max(1);
    let safe_height = (height.floor() as usize).max(1);
    // 缓存与主滚动视图都是帧级状态：本次调用结束即丢弃，不在帧间复用。
    let mut context = LayoutContext {
        viewport: LayoutViewport {
            width: safe_width,
            height: safe_height,
        },
        render_cache: HashMap::new(),
        request_render,
        primary_scroll_view: None,
    };
    let root_box = layout_component(
        &mut context,
        &root,
        0,
        0,
        safe_width as f64,
        Some(safe_height as f64),
        LayoutRect::new(0, 0, safe_width, safe_height),
    );
    // 屏幕缓冲初始化为空行；paint_box 按绘制顺序逐行写入或合成。
    let mut lines = vec![String::new(); safe_height];
    paint_box(&root_box, &mut lines, safe_width);
    LayoutFrame {
        root: root_box,
        width: safe_width,
        height: safe_height,
        lines,
        primary_scroll_view: context.primary_scroll_view,
    }
}

/// `containsPoint(rect, x, y)`：半开区间判定 `[x, x+width) × [y, y+height)`。
///
/// 右/下边界本身不算命中；`width`/`height` 为 0 时永远不命中。坐标为负时按同一规则比较
/// （滚动内容可能位于屏幕上方）。
fn contains_point(rect: LayoutRect, x: isize, y: isize) -> bool {
    x >= rect.x
        && x < rect.x + rect.width as isize
        && y >= rect.y
        && y < rect.y + rect.height as isize
}

/// `getLayoutBoxesAt(frame, x, y)`：从最深的组件到布局根的命中路径。
///
/// 命中判定使用 `clip`（不是 `rect`）：被祖先裁剪掉的区域不会命中；根盒若命中也会包含。
/// 结果按 `layer` 降序、再按深度降序排序；由于 [`LayoutBox::layer`] 恒为 0，实际顺序就是
/// 深度降序（最深处在前）。
///
/// 共享同一 `SharedComponent` 的多个盒子都可能出现在结果里，调用方需要按组件身份去重
/// （`alt_screen.rs` 的鼠标派发用组件地址去重）。
pub fn get_layout_boxes_at(frame: &LayoutFrame, x: isize, y: isize) -> Vec<&LayoutBox> {
    fn visit<'a>(
        box_: &'a LayoutBox,
        depth: usize,
        x: isize,
        y: isize,
        result: &mut Vec<(&'a LayoutBox, usize)>,
    ) {
        if !contains_point(box_.clip, x, y) {
            return;
        }
        result.push((box_, depth));
        for child in &box_.children {
            visit(child, depth + 1, x, y, result);
        }
    }
    let mut result: Vec<(&LayoutBox, usize)> = Vec::new();
    visit(&frame.root, 0, x, y, &mut result);
    result.sort_by(|a, b| b.0.layer.cmp(&a.0.layer).then(b.1.cmp(&a.1)));
    result.into_iter().map(|(box_, _)| box_).collect()
}

/// `getScrollViewBox(frame, scrollView)`：DFS 找第一个匹配该滚动视图的盒子。
///
/// 匹配用 `Rc::ptr_eq`（对应 TS 的 `===` 引用相等），不比较 `primary()`、滚动位置等内容。
/// 前序遍历：外层滚动视图先于内层访问；未挂到该帧的滚动视图返回 `None`。
pub fn get_scroll_view_box<'a>(
    frame: &'a LayoutFrame,
    scroll_view: &Rc<dyn ScrollLayoutState>,
) -> Option<&'a LayoutBox> {
    fn visit<'a>(
        box_: &'a LayoutBox,
        scroll_view: &Rc<dyn ScrollLayoutState>,
    ) -> Option<&'a LayoutBox> {
        if let Some(candidate) = box_.scroll_view.as_ref()
            && Rc::ptr_eq(candidate, scroll_view)
        {
            return Some(box_);
        }
        for child in &box_.children {
            if let Some(found) = visit(child, scroll_view) {
                return Some(found);
            }
        }
        None
    }
    visit(&frame.root, scroll_view)
}

/// `getScrollViewsAt(frame, x, y)`：该点下方所有滚动视图（最深者在前）。
///
/// 与 [`get_layout_boxes_at`] 不同：除了 `clip` 命中，还要求滚动盒的 `rect` 也包含该点；
/// 结果只按深度降序排序，**不看** `layer`。嵌套滚动视图会全部返回（由深到浅），
/// `alt_screen.rs` 的滚轮处理按这个顺序做链式滚动，文本选择则取第一个（最深的）。
pub fn get_scroll_views_at(
    frame: &LayoutFrame,
    x: isize,
    y: isize,
) -> Vec<Rc<dyn ScrollLayoutState>> {
    fn visit(
        box_: &LayoutBox,
        depth: usize,
        x: isize,
        y: isize,
        result: &mut Vec<(Rc<dyn ScrollLayoutState>, usize)>,
    ) {
        if !contains_point(box_.clip, x, y) {
            return;
        }
        if let Some(scroll_view) = box_.scroll_view.as_ref()
            && contains_point(box_.rect, x, y)
        {
            result.push((Rc::clone(scroll_view), depth));
        }
        for child in &box_.children {
            visit(child, depth + 1, x, y, result);
        }
    }
    let mut result = Vec::new();
    visit(&frame.root, 0, x, y, &mut result);
    result.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    result
        .into_iter()
        .map(|(scroll_view, _)| scroll_view)
        .collect()
}

/// 转发到 [`crate::text::normalize_terminal_output`]（对应 TS `utils.ts` 的
/// `normalizeTerminalOutput`）：Thai/Lao 的 AM 元音归一化、ANSI 之外的 TAB 展开为 3 空格。
///
/// 当前仓库内没有调用方：`TuiBase::apply_line_resets`（`tui.rs`）直接调用
/// `crate::text::normalize_terminal_output`，而不是本函数。行为与直接调用该函数完全相同。
pub fn normalize_line_output(line: &str) -> String {
    normalize_terminal_output(line)
}

// 单元测试对应 TS `layout.test.ts` 的布局器部分；端到端移植在 `tests/layout.rs`。测试替身
// 只实现 `Component`/`ScrollLayoutState` 的最小面，用来隔离布局器自身的行为（测量、滚动
// 写回、滚动条、命中测试），不依赖真实终端。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_node::{
        LayoutViewport as Viewport, Overscroll, ScrollLayoutNode, ScrollbarVisibility,
        StackLayoutEntry, StackLayoutNode, StackLayoutType,
    };
    use std::cell::{Cell, RefCell};

    struct Lines(Rc<Vec<String>>);

    impl Component for Lines {
        fn render(&mut self, _width: usize) -> Vec<String> {
            (*self.0).clone()
        }
    }

    fn leaf(lines: &[&str]) -> SharedComponent {
        Rc::new(RefCell::new(Lines(Rc::new(
            lines.iter().map(|s| s.to_string()).collect(),
        ))))
    }

    /// 只带布局节点的测试堆叠（布局器只读节点，不调用 `render`）。
    struct TestStack {
        node: StackLayoutNode,
    }

    impl Component for TestStack {
        /// `VStack.render` / `HStack.render` 的等价物（`components/stack.ts` 的测量模式：
        /// 不传可用尺寸，grow/shrink 全部忽略）。布局器测量嵌套堆叠时依赖它。
        fn render(&mut self, width: usize) -> Vec<String> {
            let safe_width = width.max(1);
            let viewport = LayoutViewport {
                width: safe_width,
                height: crate::layout_node::MAX_SAFE_INTEGER as usize,
            };
            let entries = visible_stack_entries(&self.node.entries, viewport);
            match self.node.layout_type {
                StackLayoutType::VStack => {
                    let rendered: Vec<Vec<String>> = entries
                        .iter()
                        .map(|entry| entry.component.borrow_mut().render(safe_width))
                        .collect();
                    let intrinsic: Vec<f64> =
                        rendered.iter().map(|lines| lines.len() as f64).collect();
                    let sizes = allocate_stack_sizes(&entries, &intrinsic, None, self.node.gap);
                    let mut lines = Vec::new();
                    for (index, child_lines) in rendered.iter().enumerate() {
                        if index > 0 {
                            for _ in 0..(self.node.gap as usize) {
                                lines.push(String::new());
                            }
                        }
                        let before = lines.len();
                        let size = sizes[index] as usize;
                        lines.extend(child_lines.iter().take(size).cloned());
                        while lines.len() < before + size {
                            lines.push(String::new());
                        }
                    }
                    lines
                }
                StackLayoutType::HStack => {
                    let intrinsic: Vec<f64> = entries
                        .iter()
                        .map(|entry| {
                            entry
                                .component
                                .borrow_mut()
                                .render(safe_width)
                                .iter()
                                .map(|line| crate::text::visible_width(line) as f64)
                                .fold(0.0f64, f64::max)
                        })
                        .collect();
                    let widths = allocate_stack_sizes(
                        &entries,
                        &intrinsic,
                        Some(safe_width as f64),
                        self.node.gap,
                    );
                    let rendered: Vec<Vec<String>> = entries
                        .iter()
                        .enumerate()
                        .map(|(index, entry)| {
                            let width = widths[index] as usize;
                            if width == 0 {
                                Vec::new()
                            } else {
                                entry.component.borrow_mut().render(width)
                            }
                        })
                        .collect();
                    let height = rendered.iter().map(Vec::len).max().unwrap_or(0);
                    let mut result = vec![String::new(); height];
                    let mut x = 0usize;
                    for (index, child_lines) in rendered.iter().enumerate() {
                        let offset = match self.node.align {
                            StackAlign::Center => height.saturating_sub(child_lines.len()) / 2,
                            StackAlign::End => height.saturating_sub(child_lines.len()),
                            StackAlign::Start | StackAlign::Stretch => 0,
                        };
                        for (row, line) in child_lines.iter().enumerate() {
                            let row = row + offset;
                            if row >= height {
                                continue;
                            }
                            result[row] = composite_tui_line(
                                &result[row],
                                line,
                                x as isize,
                                widths[index] as usize,
                                safe_width,
                            );
                        }
                        x += widths[index] as usize + self.node.gap as usize;
                    }
                    result
                }
            }
        }

        fn layout_node(&self) -> Option<LayoutNode> {
            Some(LayoutNode::Stack(self.node.clone()))
        }
    }

    fn stack(
        layout_type: StackLayoutType,
        entries: Vec<StackLayoutEntry>,
        gap: f64,
    ) -> SharedComponent {
        Rc::new(RefCell::new(TestStack {
            node: StackLayoutNode {
                layout_type,
                entries,
                gap,
                align: StackAlign::Stretch,
            },
        }))
    }

    fn vstack(entries: Vec<StackLayoutEntry>) -> SharedComponent {
        stack(StackLayoutType::VStack, entries, 0.0)
    }

    fn vstack_with_gap(entries: Vec<StackLayoutEntry>, gap: f64) -> SharedComponent {
        stack(StackLayoutType::VStack, entries, gap)
    }

    /// 可写的 [`ScrollLayoutState`] 测试替身（只做测量与滚动偏移）。
    struct TestScrollState {
        scroll_top: Cell<usize>,
        viewport_height: Cell<usize>,
        content_height: Cell<usize>,
        primary: bool,
        scrollbar: Cell<ScrollbarVisibility>,
    }

    impl TestScrollState {
        fn new(content_height: usize, scrollbar: ScrollbarVisibility) -> Self {
            TestScrollState {
                scroll_top: Cell::new(0),
                viewport_height: Cell::new(0),
                content_height: Cell::new(content_height),
                primary: false,
                scrollbar: Cell::new(scrollbar),
            }
        }
    }

    impl ScrollLayoutState for TestScrollState {
        fn scroll_top(&self) -> usize {
            self.scroll_top.get()
        }
        fn primary(&self) -> bool {
            self.primary
        }
        fn overscroll(&self) -> Overscroll {
            Overscroll::Chain
        }
        fn viewport_height(&self) -> usize {
            self.viewport_height.get()
        }
        fn get_content_width(&self, width: usize) -> usize {
            if self.scrollbar.get() == ScrollbarVisibility::Always && width > 1 {
                width - 1
            } else {
                width
            }
        }
        fn update_layout(
            &self,
            content_height: usize,
            viewport_height: usize,
            _request_render: &dyn Fn(),
        ) {
            self.content_height.set(content_height);
            self.viewport_height.set(viewport_height);
            let max_scroll = content_height.saturating_sub(viewport_height);
            if self.scroll_top.get() > max_scroll {
                self.scroll_top.set(max_scroll);
            }
        }
        fn scrollbar(&self) -> ScrollbarVisibility {
            self.scrollbar.get()
        }
        fn is_scrollbar_visible(&self) -> bool {
            match self.scrollbar.get() {
                ScrollbarVisibility::Hidden => false,
                ScrollbarVisibility::Auto => self.content_height.get() > self.viewport_height.get(),
                ScrollbarVisibility::Always => self.viewport_height.get() > 0,
            }
        }
    }

    struct TestScroll {
        child: SharedComponent,
        state: Rc<TestScrollState>,
    }

    impl Component for TestScroll {
        fn render(&mut self, width: usize) -> Vec<String> {
            self.child.borrow_mut().render(width)
        }

        fn layout_node(&self) -> Option<LayoutNode> {
            Some(LayoutNode::Scroll(ScrollLayoutNode {
                component: Rc::clone(&self.child),
                state: Rc::clone(&self.state) as Rc<dyn ScrollLayoutState>,
            }))
        }
    }

    fn scroll_component(lines: Vec<String>, state: Rc<TestScrollState>) -> SharedComponent {
        let child: SharedComponent = Rc::new(RefCell::new(Lines(Rc::new(lines))));
        scroll_over(child, state)
    }

    /// 以任意组件为内容的滚动视图（TS `new ScrollView(child)`）。
    fn scroll_over(child: SharedComponent, state: Rc<TestScrollState>) -> SharedComponent {
        Rc::new(RefCell::new(TestScroll { child, state }))
    }

    /// 记录 `render` 次数的叶子（TS `layout.test.ts` 的 `renderCount` 断言用）。
    struct CountingLines {
        lines: Vec<String>,
        renders: Rc<Cell<usize>>,
    }

    impl Component for CountingLines {
        fn render(&mut self, _width: usize) -> Vec<String> {
            self.renders.set(self.renders.get() + 1);
            self.lines.clone()
        }
    }

    fn visible_lines(lines: &[String]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                crate::text::strip_terminal_sequences(line)
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn allocates_vertical_grow_space_deterministically() {
        let mut top = StackLayoutEntry::new(leaf(&["top"]));
        top.basis = Some(1.0);
        top.shrink = Some(0.0);
        let mut body = StackLayoutEntry::new(leaf(&["body"]));
        body.basis = Some(0.0);
        body.grow = Some(1.0);
        let frame = render_layout_frame(vstack(vec![top, body]), 10.0, 4.0, &|| {});
        let heights: Vec<usize> = frame.root.children.iter().map(|c| c.rect.height).collect();
        assert_eq!(heights, vec![1, 3]);
        assert_eq!(visible_lines(&frame.lines), vec!["top", "body", "", ""]);
    }

    #[test]
    fn shrinks_entries_to_their_minimum_sizes() {
        let mut a = StackLayoutEntry::new(leaf(&["a1", "a2", "a3"]));
        a.shrink = Some(1.0);
        a.min_size = Some(1.0);
        let mut b = StackLayoutEntry::new(leaf(&["b1", "b2", "b3"]));
        b.shrink = Some(0.0);
        let frame = render_layout_frame(vstack(vec![a, b]), 10.0, 4.0, &|| {});
        let heights: Vec<usize> = frame.root.children.iter().map(|c| c.rect.height).collect();
        assert_eq!(heights, vec![1, 3]);
        assert_eq!(visible_lines(&frame.lines), vec!["a1", "b1", "b2", "b3"]);
    }

    #[test]
    fn includes_nested_minimum_sizes_in_intrinsic_measurement() {
        let mut selector = StackLayoutEntry::new(leaf(&["selector"]));
        selector.min_size = Some(3.0);
        let mut footer = StackLayoutEntry::new(leaf(&["footer"]));
        footer.min_size = Some(1.0);
        let dock = vstack(vec![
            StackLayoutEntry::new(leaf(&["top1", "top2", "top3"])),
            selector,
            StackLayoutEntry::new(leaf(&["below"])),
            footer,
        ]);
        let mut body = StackLayoutEntry::new(leaf(&["body"]));
        body.basis = Some(0.0);
        body.grow = Some(1.0);
        body.min_size = Some(1.0);
        let mut dock_entry = StackLayoutEntry::new(dock);
        dock_entry.min_size = Some(1.0);
        let frame = render_layout_frame(vstack(vec![body, dock_entry]), 10.0, 9.0, &|| {});
        assert_eq!(
            visible_lines(&frame.lines),
            vec![
                "body", "top1", "top2", "top3", "selector", "", "", "below", "footer"
            ]
        );
    }

    #[test]
    fn composes_horizontal_children_at_allocated_widths() {
        let mut left = StackLayoutEntry::new(leaf(&["left"]));
        left.basis = Some(6.0);
        left.shrink = Some(0.0);
        let mut right = StackLayoutEntry::new(leaf(&["right"]));
        right.basis = Some(6.0);
        right.shrink = Some(0.0);
        let frame = render_layout_frame(
            stack(StackLayoutType::HStack, vec![left, right], 0.0),
            12.0,
            1.0,
            &|| {},
        );
        assert_eq!(visible_lines(&frame.lines), vec!["left  right"]);
    }

    #[test]
    fn does_not_paint_zero_width_horizontal_children() {
        let mut hidden = StackLayoutEntry::new(leaf(&["hidden"]));
        hidden.basis = Some(0.0);
        hidden.shrink = Some(0.0);
        let mut shown = StackLayoutEntry::new(leaf(&["shown"]));
        shown.basis = Some(0.0);
        shown.grow = Some(1.0);
        let frame = render_layout_frame(
            stack(StackLayoutType::HStack, vec![hidden, shown], 0.0),
            5.0,
            1.0,
            &|| {},
        );
        assert_eq!(visible_lines(&frame.lines), vec!["shown"]);
    }

    #[test]
    fn omits_gaps_around_invisible_entries() {
        let a = StackLayoutEntry::new(leaf(&["one"]));
        let mut hidden = StackLayoutEntry::new(leaf(&["hidden"]));
        hidden.visible = Some(Rc::new(|_: Viewport| false));
        let b = StackLayoutEntry::new(leaf(&["two"]));
        // TS `omits gaps around invisible entries`：`stack.render(10)` 得到 `["one", "", "two"]`
        // —— gap 只出现在可见条目之间。
        let frame =
            render_layout_frame(vstack_with_gap(vec![a, hidden, b], 1.0), 10.0, 3.0, &|| {});
        assert_eq!(visible_lines(&frame.lines), vec!["one", "", "two"]);
    }

    #[test]
    fn updates_scroll_layout_and_respects_viewport_height() {
        let state = Rc::new(TestScrollState::new(6, ScrollbarVisibility::Hidden));
        let root = scroll_component(
            ["1", "2", "3", "4", "5", "6"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            Rc::clone(&state),
        );
        let frame = render_layout_frame(root, 10.0, 3.0, &|| {});
        assert_eq!(state.content_height.get(), 6);
        assert_eq!(state.viewport_height.get(), 3);
        assert_eq!(visible_lines(&frame.lines), vec!["1", "2", "3"]);
        let primary = frame.primary_scroll_view.as_ref().expect("primary");
        assert!(Rc::ptr_eq(
            primary,
            &(Rc::clone(&state) as Rc<dyn ScrollLayoutState>)
        ));
    }

    #[test]
    fn hit_testing_returns_deepest_first() {
        let frame = render_layout_frame(
            vstack(vec![StackLayoutEntry::new(leaf(&["x"]))]),
            4.0,
            2.0,
            &|| {},
        );
        let boxes = get_layout_boxes_at(&frame, 0, 0);
        assert_eq!(boxes.len(), 2);
        assert!(boxes[0].children.is_empty());
        assert!(get_layout_boxes_at(&frame, 0, 5).is_empty());
    }

    #[test]
    fn scrollbar_geometry_matches_thumb_formula() {
        for (content_height, expected_thumb) in [(21usize, 19usize), (40, 10), (100, 4), (400, 2)] {
            let state = Rc::new(TestScrollState::new(
                content_height,
                ScrollbarVisibility::Auto,
            ));
            let lines: Vec<String> = (0..content_height).map(|_| "x".to_string()).collect();
            let root = scroll_component(lines, Rc::clone(&state));
            render_layout_frame(Rc::clone(&root), 6.0, 20.0, &|| {});
            state.scroll_top.set(1);
            let frame = render_layout_frame(root, 6.0, 20.0, &|| {});
            let geometry = get_scrollbar_geometry(&frame.root).expect("visible");
            assert_eq!(
                geometry.thumb_height, expected_thumb,
                "content {content_height}"
            );
        }
    }

    #[test]
    fn always_scrollbar_reserves_one_column() {
        let state = Rc::new(TestScrollState::new(2, ScrollbarVisibility::Always));
        let root = scroll_component(vec!["123456".into(), "7".into()], Rc::clone(&state));
        let frame = render_layout_frame(root, 6.0, 2.0, &|| {});
        assert_eq!(frame.root.children[0].rect.width, 5);
        assert!(
            visible_lines(&frame.lines)
                .iter()
                .all(|line| line.ends_with('\u{2503}'))
        );
    }

    #[test]
    fn paints_only_clipped_rows_from_content() {
        let state = Rc::new(TestScrollState::new(5, ScrollbarVisibility::Hidden));
        let root = scroll_component(
            ["one", "two", "three", "four", "five"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            Rc::clone(&state),
        );
        render_layout_frame(Rc::clone(&root), 10.0, 2.0, &|| {});
        state.scroll_top.set(3);
        let frame = render_layout_frame(root, 10.0, 2.0, &|| {});
        assert_eq!(visible_lines(&frame.lines), vec!["four", "five"]);
    }

    #[test]
    fn strips_osc133_zone_prefixes() {
        assert_eq!(strip_osc133_zone_prefix("\u{1b}]133;A\u{7}text"), "text");
        assert_eq!(
            strip_osc133_zone_prefix("\u{1b}]133;A\u{1b}\\\u{1b}]133;B\u{7}x"),
            "x"
        );
        assert_eq!(
            strip_osc133_zone_prefix("\u{1b}]133;D\u{7}text"),
            "\u{1b}]133;D\u{7}text"
        );
        assert_eq!(strip_osc133_zone_prefix("plain"), "plain");
    }

    #[test]
    fn does_not_render_scroll_content_twice_during_stack_measurement() {
        // TS `layout.test.ts`：`does not render fixed-basis scroll content during stack measurement`
        let renders = Rc::new(Cell::new(0));
        let child: SharedComponent = Rc::new(RefCell::new(CountingLines {
            lines: vec!["one".into(), "two".into(), "three".into()],
            renders: Rc::clone(&renders),
        }));
        let state = Rc::new(TestScrollState::new(3, ScrollbarVisibility::Hidden));
        let transcript: SharedComponent = Rc::new(RefCell::new(TestScroll { child, state }));
        let mut transcript_entry = StackLayoutEntry::new(transcript);
        transcript_entry.basis = Some(0.0);
        transcript_entry.grow = Some(1.0);
        let dock = StackLayoutEntry::new(leaf(&["dock"]));
        render_layout_frame(vstack(vec![transcript_entry, dock]), 10.0, 3.0, &|| {});
        assert_eq!(renders.get(), 1);
    }

    #[test]
    fn crops_kitty_images_at_a_scroll_view_lower_boundary() {
        // TS `layout.test.ts`：`crops Kitty images at a scroll view's lower boundary`
        // Rust 侧 kitty 注册表是 `thread_local`（每个测试线程独立），无需像 TS 模块级单例那样重置。
        let image_id = 124u64;
        // `encodeKitty("AAAA", { columns: 2, rows: 3, imageId: 124, moveCursor: false })`
        let image_line = "\u{1b}_Ga=T,f=100,q=2,C=1,c=2,r=3,i=124;AAAA\u{1b}\\".to_string();
        crate::terminal_image::register_kitty_image_metadata(
            crate::terminal_image::KittyImageMetadata {
                image_id,
                columns: 2,
                rows: 3,
                width_px: 100.0,
                height_px: 100.0,
            },
        );
        let state = Rc::new(TestScrollState::new(5, ScrollbarVisibility::Hidden));
        let root = scroll_component(
            vec![
                "one".to_string(),
                "two".to_string(),
                image_line,
                String::new(),
                String::new(),
            ],
            Rc::clone(&state),
        );
        let mut transcript_entry = StackLayoutEntry::new(root);
        transcript_entry.basis = Some(0.0);
        transcript_entry.grow = Some(1.0);
        let frame = render_layout_frame(
            vstack(vec![
                transcript_entry,
                StackLayoutEntry::new(leaf(&["dock"])),
            ]),
            20.0,
            4.0,
            &|| {},
        );
        let painted = &frame.lines[2];
        assert!(painted.contains("y=0,h=34,r=1"), "got {painted:?}");
    }

    #[test]
    fn measures_nested_scroll_content_from_constrained_child_geometry() {
        // TS `layout.test.ts`：`measures nested scroll content from constrained child geometry`
        let inner_state = Rc::new(TestScrollState::new(6, ScrollbarVisibility::Hidden));
        let inner = scroll_component(
            vec![
                "1".to_string(),
                "2".to_string(),
                "3".to_string(),
                "4".to_string(),
                "5".to_string(),
                "6".to_string(),
            ],
            Rc::clone(&inner_state),
        );
        let mut inner_entry = StackLayoutEntry::new(inner);
        inner_entry.basis = Some(2.0);
        let outer_state = Rc::new(TestScrollState::new(3, ScrollbarVisibility::Hidden));
        let outer = scroll_over(
            vstack(vec![inner_entry, StackLayoutEntry::new(leaf(&["tail"]))]),
            Rc::clone(&outer_state),
        );
        render_layout_frame(outer, 10.0, 2.0, &|| {});
        assert_eq!(inner_state.viewport_height.get(), 2);
        // TS 还断言 `outer.scrollBy(10) === 9` / `outer.scrollTop === 1`——那是 ScrollView 的
        // 滚动 API（components 单元），布局器只负责把受约束的子几何写回 `update_layout`。
    }

    #[test]
    fn rebuilds_geometry_after_content_changes() {
        // TS `layout.test.ts`：`rebuilds geometry after content changes`
        struct MutableLines(Vec<String>);

        impl Component for MutableLines {
            fn render(&mut self, _width: usize) -> Vec<String> {
                self.0.clone()
            }
        }

        let component = Rc::new(RefCell::new(MutableLines(vec!["one".to_string()])));
        let shared: SharedComponent = component.clone();
        let root = vstack(vec![StackLayoutEntry::new(shared)]);
        let first = render_layout_frame(Rc::clone(&root), 10.0, 4.0, &|| {});
        component.borrow_mut().0 = vec!["one".into(), "two".into(), "three".into()];
        let second = render_layout_frame(root, 10.0, 4.0, &|| {});
        assert_eq!(
            first.root.children[0]
                .lines
                .as_ref()
                .map(|lines| lines.len()),
            Some(1)
        );
        assert_eq!(
            second.root.children[0]
                .lines
                .as_ref()
                .map(|lines| lines.len()),
            Some(3)
        );
    }

    #[test]
    fn preserves_only_the_underlying_background_beneath_overlay_scrollbar_glyphs() {
        // TS `layout.test.ts`：`preserves only the underlying background beneath overlay scrollbar glyphs`
        const BACKGROUND: &str = "\u{1b}[42m";
        const BORDER_FOREGROUND: &str = "\u{1b}[31m";

        struct Bordered;
        impl Component for Bordered {
            fn render(&mut self, width: usize) -> Vec<String> {
                (0..8)
                    .map(|_| {
                        format!(
                            "{BACKGROUND}{}{BORDER_FOREGROUND}\u{2502}\u{1b}[39m\u{1b}[49m",
                            "x".repeat(width.saturating_sub(1))
                        )
                    })
                    .collect()
            }
        }

        let state = Rc::new(TestScrollState::new(8, ScrollbarVisibility::Auto));
        let child: SharedComponent = Rc::new(RefCell::new(Bordered));
        let root = scroll_over(child, Rc::clone(&state));
        render_layout_frame(Rc::clone(&root), 6.0, 4.0, &|| {});
        state.scroll_top.set(1);
        let frame = render_layout_frame(root, 6.0, 4.0, &|| {});

        let visible: Vec<String> = frame
            .lines
            .iter()
            .map(|line| crate::text::strip_terminal_sequences(line))
            .collect();
        assert_eq!(
            visible,
            vec![
                "xxxxx\u{2502}",
                "xxxxx\u{2503}",
                "xxxxx\u{2503}",
                "xxxxx\u{2502}"
            ]
        );
        for line in &frame.lines {
            assert!(line.contains(BACKGROUND), "missing background: {line:?}");
            assert!(
                !line.contains(BORDER_FOREGROUND),
                "border foreground leaked: {line:?}"
            );
            let reset_then_background = format!("\u{1b}[0m\u{1b}]8;;\u{7}{BACKGROUND}");
            assert!(
                line.contains(&reset_then_background),
                "missing reset+background before the glyph: {line:?}"
            );
        }
    }

    #[test]
    fn updates_reserved_scrollbar_layout_at_runtime() {
        // TS `layout.test.ts`：`updates reserved scrollbar layout at runtime`
        let state = Rc::new(TestScrollState::new(2, ScrollbarVisibility::Always));
        let root = scroll_component(
            vec!["123456".to_string(), "6".to_string()],
            Rc::clone(&state),
        );
        let frame = render_layout_frame(Rc::clone(&root), 6.0, 2.0, &|| {});
        assert_eq!(
            visible_lines(&frame.lines),
            vec!["12345\u{2503}", "6    \u{2503}"]
        );
        assert_eq!(frame.root.children[0].rect.width, 5);

        state.scrollbar.set(ScrollbarVisibility::Hidden);
        let frame = render_layout_frame(root, 6.0, 2.0, &|| {});
        assert_eq!(frame.root.children[0].rect.width, 6);
        assert!(!state.is_scrollbar_visible());
    }
}
