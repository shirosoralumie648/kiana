//! tui-alt-screen.ts：备用屏（全屏）TUI。
//!
//! 持有 [`TuiBase`] 的 overlay/焦点状态机，自己实现视口渲染（布局 → 跟随末尾提示 → overlay
//! 合成 → 高度裁剪 → 选择高亮 → flash → 光标提取 → 行重置 → 全量/差分判定 → WezTerm 三遍
//! 图片 → 写出 → 快照），以及鼠标路由、文本选择、滚动条、搜索与 Kitty 图片缓存。
//!
//! ## 与 TS 的表示差异
//!
//! 1. **隐式子文档**：TS 的 `implicitDocument` 是捕获 `this` 的普通对象（`render`/`handleMouse`
//!    直接调用 `super`）。Rust 无法让组件回调借用宿主，因此用一个 `DocumentComponent`
//!    承载"按 `TuiBase.render`/`handleMouse` 语义渲染子项"的行为：渲染/鼠标派发期间把
//!    `TuiBase.children` 临时移进该文档（`TuiAltScreen::swap_children_into_document`），
//!    结束后再移回。`TuiBase.children` 始终是宿主看到的挂载列表（TS 的 `children` 数组），
//!    [`Tui::children`] 因此可以直接读写、并在 TUI 之间迁移组件（`switchTuiMode`）。
//! 2. **定时器**：TS 的 `setInterval`（选择自动滚动）、`setTimeout`（flash、滚动条自动隐藏）
//!    改成惰性截止时间，由 [`TuiAltScreen::render_decision`] 在宿主渲染循环里轮询/结算
//!    （见 [`crate::flash`]、[`ScrollLayoutState::poll_timers`] 与
//!    `TuiAltScreen::selection_auto_scroll_deadline`）。结算发生在帧开始前（`render_now`），
//!    且不补发重绘请求：一次到期只产生一帧（TS 的定时器回调只 `requestRender()` 一次）。
//! 3. **`copySelection` 回调**：TS 返回 `Promise<boolean | string>`；Rust 渲染器没有执行器，
//!    改为 **完成回调** 模型（[`CopySelectionDone`]）：宿主可以异步完成剪贴板写入，完成时调用
//!    `done(result)`，TuiAltScreen 在那一刻 flash `Copied!`/错误并请求一帧（等同 TS `await`
//!    之后的行为，UI 不阻塞）。同步实现的宿主返回 [`CopySelectionDispatch::Ready`]。
//!    [`TuiAltScreen::copy_active_selection_to_clipboard`] 因此返回
//!    [`CopySelectionStatus`]：同步结果窗口（TS `Promise<boolean>` 的同步快照）或 `Pending`；
//!    需要 TS `await` 的 bool 时用 [`TuiAltScreen::copy_active_selection_to_clipboard_with`]
//!    接收完成回调。
//! 4. **输入入口**：TS 把 `handleViewportInput` 注册成 input listener；Rust 的 listener 是
//!    `'static` 闭包，无法调用 `&mut self` 方法。因此 [`TuiAltScreen::handle_terminal_input`]
//!    先跑视口输入、再交给 [`TuiBase::handle_terminal_input`]（等价于 listener 在聚焦组件之前
//!    运行）。
//! 5. **鼠标焦点解析**：TS 的 `result.target.component` 与 `result.focusTarget` 是组件引用；
//!    Rust 的布局盒子与容器句柄是同一底层组件的不同代理对象，地址不能直接互相解析。这里在
//!    挂载树上按"底层共享组件地址"反查 [`FocusTarget`]（`alt_focus_target_of_address` /
//!    `alt_focus_target_of_shared`），语义等价。鼠标重放目标另用分发链（`MouseDispatchTrace`）
//!    记录**最内层**接收者（见 `tui.rs` 模块文档第 1 条偏差）。
//!
//! ## 谁在驱动
//!
//! - `kiana-interactive/src/tui_renderer.rs` 的 `InteractiveRenderer::Fullscreen` 持有本类型：
//!   轮询循环调用 [`TuiAltScreen::pump_pending_input`]，再由
//!   [`TuiAltScreen::take_render_decision_now`] 判定是否调用 [`Tui::render_now`]。
//! - 终端输入由宿主转交 [`TuiAltScreen::handle_terminal_input`]；停止时调用 [`Tui::stop`]。
//! - 扩展层（`kiana-extensions/src/ui/mod.rs`）通过 `Component::as_any` 向下转型取
//!   [`TuiAltScreen::terminal`]。
//!
//! ## 一次渲染的执行顺序
//!
//! 1. [`TuiAltScreen::render_decision`] 先取 `TuiBase` 的重绘决策，非 `Idle` 时直接返回；
//!    仅当它为 `Idle` 时才检查 flash、选择自动滚动与滚动视图定时器，取最早的 `Wait` 或
//!    返回 `Now`（`TuiBase` 的 `Wait`/`Now` 优先于所有定时器的截止时间）。
//! 2. [`Tui::render_now`] 先把到期的选择自动滚动、flash、滚动条定时器推进到当前时刻，
//!    再调用内部方法 `do_render`。
//! 3. `do_render`：渲染布局帧（搜索选中行变化时重排一次）→ 去掉 OSC 133 区域前缀 →
//!    解析伪光标 → 搜索高亮 → 跟随末尾提示 → overlay 合成 → 只保留底部 `height` 行 →
//!    选择高亮 → flash → 提取光标 → 行重置与按宽度截断 → 与上一帧逐行比较 →
//!    （Kitty）图片缓存处理 → 拼同步输出并写出 → 把本帧记为 `previous_screen`/`current_layout`。
//! 4. 帧内由内部组件触发的重绘请求（`pending_render`）在帧末合并为一次新的重绘请求。
//!
//! ## 输入与鼠标
//!
//! - [`TuiAltScreen::handle_terminal_input`] 先推进选择自动滚动，再按固定顺序跑视口输入
//!   （焦点事件 → 滚轮 → SGR 鼠标 → 其它鼠标序列 → 搜索/翻页/滚动快捷键）；被视口消费的
//!   输入不会交给 `TuiBase`。
//! - 未被消费的输入交给 [`TuiBase::handle_terminal_input`]（终端配色/尺寸回复、输入监听器、
//!   全局调试键、聚焦组件），随后把搜索输入组件排队的 query 交给 `update_search_query`
//!   记录；真正的索引检索与选中项计算在下一帧的 `refresh_search`（`do_render` 内）进行。
//! - 鼠标坐标在解析时统一转成 0 基：SGR 的 `x`/`y` 字段是 1 基（解析时减 1），
//!   X10 兼容格式的字节值减 33 得到 0 基。
//!
//! ## 注意事项
//!
//! - `TuiBase.children` 是宿主看到的挂载列表；渲染与鼠标派发期间它会被临时移进
//!   `DocumentComponent`（`swap_children_into_document`），结束后移回。`with_document_children`
//!   没有 panic guard：闭包 panic 时恢复不会执行，子组件会留在文档里。
//! - 定时器全部是"截止时间 + 宿主轮询"，没有后台线程；到期效果依赖宿主调用
//!   `render_now` / `render_decision` 等入口来推进。
//! - 状态通过 `Rc<RefCell<…>>` 共享，本类型不是 `Send`/`Sync`，只能在 UI 线程使用
//!   （与 `tui.rs` 的单线程前提一致）。

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use indexmap::IndexMap;

use crate::alt_screen_search::{
    AltScreenSearchComponent, AltScreenSearchIndex, AltScreenSearchMatch,
    get_alt_screen_search_match_key,
};
use crate::components::scroll_view::{ScrollView, ScrollViewFollow, ScrollViewOptions};
use crate::components::stack::{SharedHandle, wrap_child};
use crate::flash::AltScreenFlashContainer;
use crate::keybindings::get_keybindings;
use crate::keys::is_key_release;
use crate::layout::{
    LayoutBox, LayoutFrame, ScrollbarGeometry, get_layout_boxes_at, get_scroll_view_box,
    get_scroll_views_at, get_scrollbar_geometry, get_scrollbar_geometry_with, render_layout_frame,
};
use crate::layout_node::{ScrollLayoutState, ScrollViewControl, SharedComponent};
use crate::terminal_image::{
    ImageProtocol, TerminalCapabilities, delete_all_kitty_images, delete_all_kitty_placements,
    delete_kitty_image, get_capabilities, get_kitty_image_placement,
    get_kitty_image_placement_rows, is_image_line, set_capabilities,
};
use crate::text::{
    AnsiCode, extract_ansi_code, get_grapheme_cell_range, get_osc8_link_at_column,
    get_word_segments, slice_by_column, strip_terminal_sequences, trim_end_js, trim_js,
    truncate_to_width, visible_width,
};
use crate::tui::{
    CURSOR_MARKER, Component, FocusPath, FocusTarget, Focusable, MouseDispatchTrace,
    MouseFocusTarget, OverlayAnchor, OverlayHandle, OverlayMarginValue, OverlayOptions,
    RenderDecision, SizeValue, Tui, TuiBase, TuiInputListener, TuiInputListenerResult,
    TuiLifecycleHooks, TuiMode, TuiMouseButton, TuiMouseDispatchTarget, TuiMouseEvent,
    TuiMouseEventResult, TuiMouseEventType, TuiStopOptions, ViewportTui,
    begin_mouse_dispatch_trace, composite_tui_line, dispatch_mouse_event, retarget_mouse_event,
    take_mouse_dispatch_trace,
};
use crate::wheel::{WheelScrollAccelerator, WheelScrollLines};

// ---------------------------------------------------------------------------
// 常量（**逐字**，`spec/tui-core.md` §2.13.1）
// ---------------------------------------------------------------------------

const ENTER_ALT_SCREEN: &str = "\x1b[?1049h";
const EXIT_ALT_SCREEN: &str = "\x1b[?1049l";
const DISABLE_AUTOWRAP: &str = "\x1b[?7l";
const ENABLE_AUTOWRAP: &str = "\x1b[?7h";
// 两个启用序列都带 `?1004h`（焦点上报），因此 `FOCUS_IN`/`FOCUS_OUT` 会出现在输入流里；
// `DISABLE_MOUSE` 同时关闭按键、motion、焦点上报与 SGR 扩展。
const ENABLE_BUTTON_MOTION_MOUSE: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1004h\x1b[?1006h";
const ENABLE_ALL_MOTION_MOUSE: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1004h\x1b[?1006h";
const DISABLE_MOUSE: &str = "\x1b[?1006l\x1b[?1004l\x1b[?1003l\x1b[?1002l\x1b[?1000l";
const FOCUS_IN: &str = "\x1b[I";
const FOCUS_OUT: &str = "\x1b[O";
const BEGIN_SYNCHRONIZED_OUTPUT: &str = "\x1b[?2026h";
const END_SYNCHRONIZED_OUTPUT: &str = "\x1b[?2026l";
const PAGE_SCROLL_OVERLAP: usize = 4;
const ALT_WHEEL_SCROLL_MULTIPLIER: f64 = 5.0;
// 不可见 Kitty 图片缓存的上限：条目数、传输字节数、估算解码字节数任一超出即按 LRU 淘汰
// （见 `prepare_kitty_screen`）。
const MAX_CACHED_OFFSCREEN_KITTY_IMAGES: usize = 16;
const MAX_CACHED_OFFSCREEN_KITTY_TRANSMISSION_BYTES: usize = 32 * 1024 * 1024;
const MAX_CACHED_OFFSCREEN_KITTY_DECODED_BYTES: f64 = 64.0 * 1024.0 * 1024.0;
const DOUBLE_CLICK_INTERVAL_MS: u128 = 500;
const COPY_ERROR_FLASH_DURATION_MS: f64 = 5000.0;
const AUTO_SCROLL_INTERVAL_MS: u64 = 50;
// iTerm2 默认的"词字符"集合（`/ - + \ ~ _ .` 本身可作为连接符）。
const TERMINAL_WORD_SELECTION_JOINERS: [&str; 7] = ["/", "-", "+", "\\", "~", "_", "."];

// ---------------------------------------------------------------------------
// 样式/回调类型
// ---------------------------------------------------------------------------

/// 文本样式函数（TS `(text: string) => string`）。
pub type AltStyleFn = Rc<dyn Fn(&str) -> String>;
/// 搜索导航按钮样式（TS `(text, hovered) => string`）。
pub type SearchNavigationButtonStyle = Rc<dyn Fn(&str, bool) -> String>;
/// 跳到底部指示器（TS `() => string`）。
pub type ScrollToEndIndicatorFn = Rc<dyn Fn() -> String>;
/// OSC 8 链接激活（TS `(url: string) => void`）。
pub type OpenUrlFn = Rc<dyn Fn(&str)>;
/// 右键粘贴（TS `() => void`）。
pub type RightClickPasteFn = Rc<dyn Fn()>;

/// `copySelection(text)` 的结果（TS `true | false | string`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopySelectionResult {
    /// `true`：复制成功。
    Ok,
    /// `false`：通用失败（显示 `Copy failed`）。
    Failed,
    /// 字符串：失败原因（原样 flash）。
    Error(String),
}

/// `copySelection(text)` 的异步完成回调（TS `Promise<boolean | string>` 的 resolve）。
///
/// 宿主在剪贴板写入真正结束后调用一次；TuiAltScreen 随后按 TS 的
/// `await copySelection(text)` 语义 flash `Copied!`/错误消息并请求一帧。重复调用忽略。
pub type CopySelectionDone = Rc<dyn Fn(CopySelectionResult)>;

/// `copySelection(text)` 的返回（TS `Promise<boolean | string>` 的两种状态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopySelectionDispatch {
    /// 已经完成（等价于已 resolve 的 Promise）：`copyTextToClipboard` 立即 flash 并返回结果。
    Ready(CopySelectionResult),
    /// 已接管 [`CopySelectionDone`]，稍后调用（等价于 pending 的 Promise）；UI 不阻塞。
    Pending,
}

/// 复制回调（TS `(text) => Promise<boolean | string>`）。
///
/// Rust 的 TUI 是同步渲染器，没有执行器去 await Promise：宿主拿到 [`CopySelectionDone`]
/// 后可以自由地异步完成剪贴板写入，完成时调用它（flash 与重绘在那一刻发生，语义等同 TS
/// `await` 之后的行为）。同步实现的宿主直接返回 [`CopySelectionDispatch::Ready`]，不要
/// 同时调用 `done`。
pub type CopySelectionFn = Rc<dyn Fn(&str, CopySelectionDone) -> CopySelectionDispatch>;

/// `copyActiveSelectionToClipboard()` 的返回（TS `Promise<boolean>` 的同步快照）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopySelectionStatus {
    /// 已完成：`true` = 复制成功（TS 的 `await` 结果）。
    Complete(bool),
    /// 复制回调登记了异步完成，结果稍后通过 [`CopySelectionDone`] 交付。
    Pending,
}

impl CopySelectionStatus {
    /// 同步路径的结果；`Pending` 返回 `None`。
    pub fn complete(self) -> Option<bool> {
        match self {
            CopySelectionStatus::Complete(ok) => Some(ok),
            CopySelectionStatus::Pending => None,
        }
    }
}

/// 等待复制最终 `bool` 的完成槽（TS `Promise<boolean>` 的 resolve；同步路径返回前取出调用）。
type CopyCompletionSlot = Rc<RefCell<Option<Box<dyn FnOnce(bool)>>>>;

/// 把复制结果映射成 flash 消息与时长（TS `copyTextToClipboard` 的三元表达式）。
///
/// 成功不指定时长（使用 flash 容器的默认 1000 ms）；两种失败都用
/// `COPY_ERROR_FLASH_DURATION_MS`（5000 ms），`Error` 的原因字符串原样显示。
fn copy_selection_flash(result: &CopySelectionResult) -> (String, Option<f64>) {
    match result {
        CopySelectionResult::Ok => ("Copied!".to_string(), None),
        CopySelectionResult::Error(message) => {
            (message.clone(), Some(COPY_ERROR_FLASH_DURATION_MS))
        }
        CopySelectionResult::Failed => (
            "Copy failed".to_string(),
            Some(COPY_ERROR_FLASH_DURATION_MS),
        ),
    }
}

// ---------------------------------------------------------------------------
// 选项（§2.13.2）
// ---------------------------------------------------------------------------

/// `TuiAltScreen` 的选项（TS `TuiAltScreenOptions`）。
///
/// 构造时对缺省值求值一次（TuiAltScreen 持有的是求值后的值，运行期改本结构不生效）。
#[derive(Clone, Default)]
pub struct TuiAltScreenOptions {
    /// 每个滚轮事件移动的逻辑行数（默认 1）；`WheelScrollLines::Auto` 按速度加速。
    /// 固定行数下加速参数不生效。
    pub wheel_scroll_lines: Option<WheelScrollLines>,
    /// 捕获鼠标事件用于视口滚动与应用内文本选择（默认 true）。
    pub mouse: Option<bool>,
    /// 非当前搜索匹配的样式（默认 `\x1b[4m…\x1b[24m`）。
    pub search_match_style: Option<AltStyleFn>,
    /// 当前搜索匹配的样式（默认 `\x1b[1;7m…\x1b[22;27m`）。
    pub search_current_match_style: Option<AltStyleFn>,
    /// 搜索导航按钮样式（默认恒等）。
    pub search_navigation_button_style: Option<SearchNavigationButtonStyle>,
    /// 跟随末尾的主滚动视图滚离末尾时，最后一行上的可点击标签。
    pub scroll_to_end_indicator: Option<ScrollToEndIndicatorFn>,
    /// 主键单击激活 OSC 8 超链接。
    pub open_url: Option<OpenUrlFn>,
    /// 未修饰的次键按下（仅 Windows 启用）。
    pub on_right_click_paste: Option<RightClickPasteFn>,
    /// 鼠标松开时自动复制选中文本（默认 true）。
    pub copy_on_select: Option<bool>,
    /// 复制选中文本（缺省时走 OSC 52）。回调可以同步返回结果，也可以登记
    /// [`CopySelectionDone`] 异步完成（见模块文档第 3 条）。
    pub copy_selection: Option<CopySelectionFn>,
}

impl std::fmt::Debug for TuiAltScreenOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TuiAltScreenOptions")
            .field("wheel_scroll_lines", &self.wheel_scroll_lines)
            .field("mouse", &self.mouse)
            .field(
                "search_match_style",
                &self.search_match_style.as_ref().map(|_| "<fn>"),
            )
            .field(
                "search_current_match_style",
                &self.search_current_match_style.as_ref().map(|_| "<fn>"),
            )
            .field(
                "search_navigation_button_style",
                &self.search_navigation_button_style.as_ref().map(|_| "<fn>"),
            )
            .field(
                "scroll_to_end_indicator",
                &self.scroll_to_end_indicator.as_ref().map(|_| "<fn>"),
            )
            .field("open_url", &self.open_url.as_ref().map(|_| "<fn>"))
            .field(
                "on_right_click_paste",
                &self.on_right_click_paste.as_ref().map(|_| "<fn>"),
            )
            .field("copy_on_select", &self.copy_on_select)
            .field(
                "copy_selection",
                &self.copy_selection.as_ref().map(|_| "<fn>"),
            )
            .finish()
    }
}

// ---------------------------------------------------------------------------
// 内部状态
// ---------------------------------------------------------------------------

/// Kitty 图片缓存项（TS `CachedKittyImage`）。
#[derive(Debug, Clone, Copy)]
struct CachedKittyImage {
    transmission_generation: u64,
    transmission_bytes: usize,
    estimated_decoded_bytes: f64,
}

/// 选择点（TS `SelectionPoint`）。
#[derive(Clone)]
struct SelectionPoint {
    row: usize,
    col: usize,
    scroll_view: Option<Rc<dyn ScrollLayoutState>>,
    /// 该点落在终端单元格之间（而不是某个单元格上）。
    boundary: bool,
}

impl SelectionPoint {
    fn plain(row: usize, col: usize) -> Self {
        SelectionPoint {
            row,
            col,
            scroll_view: None,
            boundary: false,
        }
    }
}

fn same_scroll_view(
    left: &Option<Rc<dyn ScrollLayoutState>>,
    right: &Option<Rc<dyn ScrollLayoutState>>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(a), Some(b)) => Rc::ptr_eq(a, b),
        _ => false,
    }
}

/// 选择区间（TS `SelectionRange`）。
#[derive(Clone)]
struct SelectionRange {
    start: SelectionPoint,
    end: SelectionPoint,
}

/// 选择粒度（TS `SelectionGranularity`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionGranularity {
    Character,
    Word,
    Line,
}

/// 多击目标（TS `ClickTarget`）。
struct ClickTarget {
    timestamp: Instant,
    count: u8,
    row: usize,
    scroll_view: Option<Rc<dyn ScrollLayoutState>>,
    word_start: usize,
    word_end: usize,
}

/// SGR 鼠标事件（TS `SgrMouseEvent`）。
#[derive(Debug, Clone, Copy)]
struct SgrMouseEvent {
    button: i64,
    x: isize,
    y: isize,
    release: bool,
}

/// 滚轮事件（TS `WheelEvent`）。
#[derive(Debug, Clone, Copy)]
struct WheelEvent {
    direction: i8,
    x: isize,
    y: isize,
    button: i64,
}

/// 滚动条拖动（TS `ScrollbarDrag`）。
#[derive(Clone)]
struct ScrollbarDrag {
    scroll_view: Rc<dyn ScrollLayoutState>,
    grab_offset: isize,
}

/// 滚动条命中（TS `ScrollbarTarget`）。
struct ScrollbarTarget {
    scroll_view: Rc<dyn ScrollLayoutState>,
    geometry: ScrollbarGeometry,
}

/// 跳到底部指示器矩形（TS `ScrollToEndIndicatorRect`）。
#[derive(Debug, Clone, Copy)]
struct ScrollToEndIndicatorRect {
    row: usize,
    column: isize,
    width: usize,
}

/// 搜索选择模式（TS `SearchSelectionMode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchSelectionMode {
    Query,
    Retain,
    Next,
    Previous,
}

/// 活动搜索（TS `ActiveSearch`）。
struct ActiveSearch {
    component: SharedComponent,
    index: AltScreenSearchIndex,
    overlay: Option<Box<dyn OverlayHandle>>,
    query: String,
    matches: Rc<Vec<AltScreenSearchMatch>>,
    selected_index: isize,
    selected_key: Option<String>,
    anchor_row: usize,
    selection_mode: SearchSelectionMode,
}

/// 搜索高亮区间（TS `SearchHighlightRange`）。
#[derive(Debug, Clone, Copy)]
struct SearchHighlightRange {
    start_col: usize,
    end_col: usize,
    current: bool,
}

/// 组件点击计数（TS `lastComponentClick`）。
struct ComponentClick {
    timestamp: Instant,
    count: u8,
    key: MouseTargetKey,
    x: isize,
    y: isize,
}

/// 鼠标目标身份（**最内层**命中组件的地址；复刻 TS `target.component` 的引用相等）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MouseTargetKey(usize);

/// 可重放的分发目标（TS `result.target`）。
///
/// TS 的 target 是**最内层**产生结果的组件引用；Rust 的分发结果不携带引用（见 `tui.rs`
/// 模块文档第 1 条偏差），因此分发时用 [`begin_mouse_dispatch_trace`] 记录接收者链，
/// 这里保存"起点组件 + 到最内层的子项地址链 + 各层几何"，重放时沿链下钻
/// （[`dispatch_along_path`]），与原实现"重新经过起点容器、按当前指针位置再路由"不同。
#[derive(Clone)]
enum MouseTarget {
    /// 组件（布局盒子或隐式文档子项）。
    Component {
        component: SharedComponent,
        /// 从 `component` 到最内层命中的子项地址链（`path[i]` 是第 i+1 层接收者）。
        path: Vec<usize>,
        /// 各层几何：`geometries[0]` 是起点的，`geometries[i + 1]` 对应 `path[i]`。
        geometries: Vec<TuiMouseDispatchTarget>,
        fallback: FocusTarget,
        key: MouseTargetKey,
    },
    /// overlay 组件：按 id 重新取出组件并分发（几何用于重放时重建局部坐标）。
    Overlay {
        id: usize,
        path: Vec<usize>,
        geometries: Vec<TuiMouseDispatchTarget>,
        key: MouseTargetKey,
    },
}

impl MouseTarget {
    fn key(&self) -> MouseTargetKey {
        match self {
            MouseTarget::Component { key, .. } | MouseTarget::Overlay { key, .. } => *key,
        }
    }
}

/// 一次鼠标分发结果（TS `TuiMouseDispatchResult` + 目标组件）。
#[derive(Clone)]
struct DispatchOutcome {
    result: TuiMouseEventResult,
    target: MouseTarget,
}

/// 共享包装（隐式子文档的 [`SharedProxy`] 与 Stack/ScrollView 的 [`SharedHandle`]）的底层句柄。
///
/// 它们的 `handleMouse` 直接调用底层组件的 `handleMouse`（中间没有 `dispatchMouseEvent`
/// 调用），所以重放导航经过它们时不消耗路径项。
fn shared_wrapper_inner(component: &mut dyn Component) -> Option<SharedComponent> {
    let any = component.as_any_mut()?;
    if let Some(proxy) = any.downcast_mut::<SharedProxy>() {
        return Some(Rc::clone(proxy.inner()));
    }
    if let Some(handle) = any.downcast_mut::<SharedHandle>() {
        return Some(Rc::clone(handle.inner()));
    }
    None
}

/// 在容器的直接子项里按地址查找（`path` 链的一步）。
fn find_child_mut_by_address<'a>(
    component: &'a mut dyn Component,
    address: usize,
) -> Option<&'a mut (dyn Component + 'static)> {
    let children = component.mouse_children_mut()?;
    children
        .iter_mut()
        .find(|child| child.as_ref() as *const dyn Component as *const () as usize == address)
        .map(|child| child.as_mut())
}

/// 把事件沿地址链重放到最内层命中组件（TS `dispatchMouseToTarget` 的语义）。
///
/// `path[i]` 是第 i+1 层接收者在第 i 层组件里的地址，`geometries[i]` 是第 i 层的几何。
/// 不变量：`geometries.len() == path.len() + 1`（由 [`target_path_from_trace`] 保证）。
/// 共享句柄代理（`SharedProxy`/`SharedHandle`）直接转发 `handleMouse`，不消耗路径项。
/// 终点层已脱离树时退回仍可解析的最深一层：TS 持有组件引用、会继续投递给已脱离树的对象，
/// Rust 的独占子项在移除后已释放，无法等价保留。
fn dispatch_along_path(
    component: &mut dyn Component,
    path: &[usize],
    geometries: &[TuiMouseDispatchTarget],
    index: usize,
    event: &TuiMouseEvent,
) -> Option<TuiMouseEventResult> {
    if !path.is_empty()
        && let Some(inner) = shared_wrapper_inner(component)
    {
        let mut guard = inner.borrow_mut();
        return dispatch_along_path(&mut *guard, path, geometries, index, event);
    }
    let Some((&address, rest)) = path.split_first() else {
        let local = retarget_mouse_event(event, &geometries[index]);
        return dispatch_mouse_event(component, &local);
    };
    if let Some(child) = find_child_mut_by_address(component, address) {
        return dispatch_along_path(child, rest, geometries, index + 1, event);
    }
    let local = retarget_mouse_event(event, &geometries[index]);
    dispatch_mouse_event(component, &local)
}

/// 分发链（**最内层优先**，见 [`MouseDispatchTrace`]）→ (地址链, 各层几何, 目标身份)。
///
/// `trace` 由 [`dispatch_mouse_event`] 在返回结果时按由内向外压栈，最后一条是分发起点；
/// 这里反转为由外向内。返回的 `path[i]` 是第 i+1 层接收者的地址，`path` 不含起点，
/// `geometries[0]` 是起点的几何，因此始终满足 `geometries.len() == path.len() + 1`。
/// 未开启记录（不应发生）时退回"起点自身、空路径"。
fn target_path_from_trace(
    start_address: usize,
    start_geometry: TuiMouseDispatchTarget,
    trace: Vec<MouseDispatchTrace>,
) -> (Vec<usize>, Vec<TuiMouseDispatchTarget>, MouseTargetKey) {
    let mut levels = trace;
    levels.reverse(); // 由外向内
    let Some(outermost) = levels.first() else {
        // 未开启记录（正常不会发生）：退回起点自身。
        return (
            Vec::new(),
            vec![start_geometry],
            MouseTargetKey(start_address),
        );
    };
    debug_assert_eq!(
        outermost.address, start_address,
        "分发链起点必须是本次接收者"
    );
    let key = MouseTargetKey(levels.last().expect("非空").address);
    let geometries = levels.iter().map(|entry| entry.target).collect();
    let path = levels[1..].iter().map(|entry| entry.address).collect();
    (path, geometries, key)
}

/// `getGraphemeCellRange(line, col)` 的起点（负列号视为无效，与 TS 返回 `undefined` 一致）。
fn grapheme_start_at(line: &str, col: isize) -> Option<isize> {
    if col < 0 {
        return None;
    }
    get_grapheme_cell_range(line, col as usize).map(|range| range.start as isize)
}

/// `getGraphemeCellRange(line, col)` 的终点（负列号视为无效，与 TS 返回 `undefined` 一致）。
fn grapheme_end_at(line: &str, col: isize) -> Option<isize> {
    if col < 0 {
        return None;
    }
    get_grapheme_cell_range(line, col as usize).map(|range| range.end as isize)
}

/// 共享状态（TuiAltScreen 与生命周期钩子共用）。
///
/// TS 的钩子是子类方法（可直接访问全部字段）；Rust 的钩子对象无法借用宿主，因此把钩子
/// 需要的渲染/选择状态放进 `Rc<RefCell<AltState>>`。
///
/// 字段按用途分组：
/// - 渲染快照：`previous_screen` 及其宽高用于逐行差分与全量重绘判定；`current_layout` 是最近
///   一帧的布局帧，滚动、搜索、选择、滚动条都从它取盒子和内容行；`last_document` 是退出备用屏
///   时写到主屏的内容。
/// - 终端/图片：`alt_screen_active` 表示是否已进入备用屏；`uploaded_kitty_images` 的迭代顺序
///   就是淘汰顺序（命中会移到末尾，见 `prepare_kitty_screen`）。
/// - 文本选择：`selection_anchor` 与 `selection_focus` 两点确定区间；`selection_granularity`
///   与 `selection_initial_range` 支持双击/三击后拖动扩展；`selection_press_active` 表示左键
///   按下尚未松开。
/// - 鼠标手势：`mouse_capture`（组件在结果里请求 capture）与 `mouse_press_target`（当前按下的
///   组件目标）优先于 overlay/布局命中测试。
/// - 搜索：`active_search` 持有搜索 overlay 与索引；`pending_query` 是输入组件回调排队的查询，
///   避免在组件回调期间再次借用 `TuiAltScreen`。
/// - 定时器：`pending_render` 只做同一帧内的重绘合并；真正唤醒宿主的是 `TuiBase.renderRequested`。
struct AltState {
    /// 上一帧写出的最终行（差分基准；没有滚动视图时也是选择文本的来源）。
    previous_screen: Vec<String>,
    /// 退出备用屏且不保留屏幕时写出的文档行（主屏恢复内容）。
    last_document: Vec<String>,
    previous_screen_width: usize,
    previous_screen_height: usize,
    /// 显式布局根的布局句柄；`None` 表示使用隐式子文档。
    layout_root_shared: Option<SharedComponent>,
    /// 最近一帧的布局帧（尚未渲染或刚被重置时为 `None`）。
    current_layout: Option<LayoutFrame>,
    alt_screen_active: bool,
    image_protocol: Option<ImageProtocol>,
    /// iTerm2 协议下进入备用屏时暂存、退出时恢复的能力（见 `before_terminal_start_alt`）。
    saved_capabilities: Option<TerminalCapabilities>,
    /// 已上传的 Kitty 图片缓存；`IndexMap` 的插入顺序用于 LRU 淘汰。
    uploaded_kitty_images: IndexMap<u64, CachedKittyImage>,
    selection_anchor: Option<SelectionPoint>,
    selection_focus: Option<SelectionPoint>,
    selection_granularity: SelectionGranularity,
    /// 双击/三击选中的初始区间；拖动时以它为基准向两侧扩展。
    selection_initial_range: Option<SelectionRange>,
    last_click: Option<ClickTarget>,
    /// 自动滚动期间最后一次指针位置（自动滚动 tick 用它重新计算焦点）。
    selection_drag_pointer: Option<(isize, isize)>,
    /// 自动滚动方向：-1 向上、1 向下、0 停止。
    selection_auto_scroll_direction: i8,
    /// 上一次自动滚动 tick 的时刻；`None` 表示时钟尚未启动。
    selection_auto_scroll_last_tick: Option<Instant>,
    selection_press_active: bool,
    scrollbar_drag: Option<ScrollbarDrag>,
    scrollbar_hover: Option<Rc<dyn ScrollLayoutState>>,
    /// 上一帧合成的"跟随末尾提示"矩形（鼠标命中测试用；每帧先清空再记录）。
    scroll_to_end_indicator_rect: Option<ScrollToEndIndicatorRect>,
    active_search: Option<ActiveSearch>,
    /// 搜索输入组件的回调排队到下一次输入分发末尾的查询。
    pending_query: Option<String>,
    /// 按下位置命中的 OSC 8 URL（仅字符粒度按下时记录）；松开且未拖动时激活。
    pressed_url: Option<String>,
    selection_dragged: bool,
    /// 组件请求 capture 后的重放目标（后续事件直接投递给它）。
    mouse_capture: Option<MouseTarget>,
    /// 本次按下的组件目标（松开时据此合成 Click）。
    mouse_press_target: Option<MouseTarget>,
    mouse_press_point: Option<(isize, isize)>,
    mouse_press_moved: bool,
    last_component_click: Option<ComponentClick>,
    /// 帧内合并用的重绘标志（只影响 `do_render` 末尾是否再排一帧）。
    pending_render: Rc<Cell<bool>>,
    flashes: Rc<RefCell<AltScreenFlashContainer>>,
}

impl AltState {
    fn new(flashes: Rc<RefCell<AltScreenFlashContainer>>) -> Self {
        AltState {
            previous_screen: Vec::new(),
            last_document: Vec::new(),
            previous_screen_width: 0,
            previous_screen_height: 0,
            layout_root_shared: None,
            current_layout: None,
            alt_screen_active: false,
            image_protocol: None,
            saved_capabilities: None,
            uploaded_kitty_images: IndexMap::new(),
            selection_anchor: None,
            selection_focus: None,
            selection_granularity: SelectionGranularity::Character,
            selection_initial_range: None,
            last_click: None,
            selection_drag_pointer: None,
            selection_auto_scroll_direction: 0,
            selection_auto_scroll_last_tick: None,
            selection_press_active: false,
            scrollbar_drag: None,
            scrollbar_hover: None,
            scroll_to_end_indicator_rect: None,
            active_search: None,
            pending_query: None,
            pressed_url: None,
            selection_dragged: false,
            mouse_capture: None,
            mouse_press_target: None,
            mouse_press_point: None,
            mouse_press_moved: false,
            last_component_click: None,
            pending_render: Rc::new(Cell::new(false)),
            flashes,
        }
    }

    /// `stopSelectionAutoScroll()`。
    fn stop_selection_auto_scroll(&mut self) {
        self.selection_auto_scroll_direction = 0;
        self.selection_auto_scroll_last_tick = None;
        self.selection_drag_pointer = None;
    }

    /// `clearTextSelection()`。
    fn clear_text_selection(&mut self) {
        self.stop_selection_auto_scroll();
        self.selection_press_active = false;
        self.selection_anchor = None;
        self.selection_focus = None;
        self.selection_granularity = SelectionGranularity::Character;
        self.selection_initial_range = None;
        self.pressed_url = None;
        self.selection_dragged = false;
    }

    /// `clearComponentMouseGesture()`。
    fn clear_component_mouse_gesture(&mut self) {
        self.mouse_capture = None;
        self.mouse_press_target = None;
        self.mouse_press_point = None;
        self.mouse_press_moved = false;
    }

    /// `setScrollbarHover(scrollView)`：同一滚动视图时无操作；切换时先把旧视图的
    /// `scrollbarActive` 置 false，再给新视图置 true。
    fn set_scrollbar_hover(&mut self, scroll_view: Option<Rc<dyn ScrollLayoutState>>) {
        let same = match (&self.scrollbar_hover, &scroll_view) {
            (None, None) => true,
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            _ => false,
        };
        if same {
            return;
        }
        if let Some(previous) = self.scrollbar_hover.take()
            && let Some(control) = previous.scroll_view_control()
        {
            control.set_scrollbar_active(false);
        }
        self.scrollbar_hover = scroll_view;
        if let Some(current) = self.scrollbar_hover.as_ref()
            && let Some(control) = current.scroll_view_control()
        {
            control.set_scrollbar_active(true);
        }
    }

    /// `stopScrollbarHover()`。
    fn stop_scrollbar_hover(&mut self) {
        self.set_scrollbar_hover(None);
    }

    /// `stopScrollbarDrag()`。
    fn stop_scrollbar_drag(&mut self) {
        self.scrollbar_drag = None;
    }
}

// ---------------------------------------------------------------------------
// 隐式文档
// ---------------------------------------------------------------------------

/// 共享句柄的透明代理（把同一个底层组件同时挂进容器与布局条目）。
///
/// 与 `components::stack::SharedHandle` 的区别：没有独立的 `focused` 镜像，`set_focused`
/// 直接写底层组件。用于「同一子组件同时出现在 `TuiBase.children` 与隐式文档里」时避免两份
/// 焦点标志互相覆盖。
struct SharedProxy {
    inner: SharedComponent,
}

impl SharedProxy {
    fn new(inner: SharedComponent) -> Self {
        SharedProxy { inner }
    }

    fn inner(&self) -> &SharedComponent {
        &self.inner
    }
}

impl Component for SharedProxy {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.inner.borrow_mut().render(width)
    }

    fn handle_input(&mut self, data: &str) {
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

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn for_each_container_child(&self, f: &mut dyn FnMut(&dyn Component) -> bool) {
        self.inner.borrow().for_each_container_child(f);
    }

    fn for_each_container_child_mut(
        &mut self,
        f: &mut dyn FnMut(&mut (dyn Component + 'static)) -> bool,
    ) {
        self.inner.borrow_mut().for_each_container_child_mut(f);
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        if self.inner.borrow_mut().as_focusable().is_some() {
            Some(self)
        } else {
            None
        }
    }

    fn layout_node(&self) -> Option<crate::layout_node::LayoutNode> {
        self.inner.borrow().layout_node()
    }
}

impl Focusable for SharedProxy {
    fn focused(&self) -> bool {
        let mut inner = self.inner.borrow_mut();
        inner
            .as_focusable()
            .map(|focusable| focusable.focused())
            .unwrap_or(false)
    }

    fn set_focused(&mut self, focused: bool) {
        if let Some(focusable) = self.inner.borrow_mut().as_focusable() {
            focusable.set_focused(focused);
        }
    }
}

/// `TuiBase.render` 的等价物：按顺序渲染全部子组件，并记录鼠标路由所需的高度。
///
/// 生命周期约束：`children` 只在 `swap_children_into_document` 与
/// `restore_children_from_document` 之间非空，其余时刻由 `TuiBase.children` 持有。
/// `handle_mouse` 依赖上一次 `render` 记录的 `mouse_layout`；宽度不匹配或尚未记录时会重新
/// 渲染子项来求高度（会实际调用组件的 `render`）。
#[derive(Default)]
struct DocumentComponent {
    children: Vec<Box<dyn Component>>,
    /// `(width, 每个子项高度)`（TS `mouseLayout`）。
    mouse_layout: Option<(usize, Vec<usize>)>,
}

impl DocumentComponent {
    fn new() -> Self {
        Self::default()
    }
}

impl Component for DocumentComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let mut heights = Vec::with_capacity(self.children.len());
        for child in &mut self.children {
            let child_lines = child.render(width);
            heights.push(child_lines.len());
            lines.extend(child_lines);
        }
        self.mouse_layout = Some((width, heights));
        lines
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if event.y < 0 || event.y as usize >= event.height {
            return None;
        }
        // 有同宽布局记录时直接用缓存高度；否则（或宽度变化）重新渲染每个子项来求高度。
        let heights: Vec<usize> = match &self.mouse_layout {
            Some((width, heights)) if *width == event.width => heights.clone(),
            _ => self
                .children
                .iter_mut()
                .map(|child| child.render(event.width).len())
                .collect(),
        };
        let mut child_y: isize = 0;
        for (index, height) in heights.into_iter().enumerate() {
            if event.y >= child_y && event.y < child_y + height as isize {
                let child = self.children.get_mut(index)?;
                let mut child_event = event.clone();
                child_event.y = event.y - child_y;
                child_event.height = height;
                // TS 的隐式文档对象没有 `handleInput`，因此焦点目标保持为命中的子控件。
                return dispatch_mouse_event(child.as_mut(), &child_event);
            }
            child_y += height as isize;
        }
        None
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {
        for child in &mut self.children {
            child.invalidate();
        }
        self.mouse_layout = None;
    }

    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        Some(&self.children)
    }

    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        Some(&mut self.children)
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

// ---------------------------------------------------------------------------
// 组件地址辅助
// ---------------------------------------------------------------------------

/// 共享组件的**底层值地址**（`Rc<RefCell<dyn Component>>` 里 `dyn Component` 的地址）。
///
/// 同一个 `Rc` 的所有克隆（包括布局条目与容器句柄）共享这个地址，因此可以跨代理对象
/// 比对组件身份（TS 的引用相等）。
fn shared_value_address(shared: &SharedComponent) -> usize {
    let borrowed = shared.borrow();
    &*borrowed as *const dyn Component as *const () as usize
}

/// [`shared_value_address`] 的不 panic 版本（组件正被外部可变借用时返回 `None`）。
fn shared_value_address_try(shared: &SharedComponent) -> Option<usize> {
    let borrowed = shared.try_borrow().ok()?;
    Some(&*borrowed as *const dyn Component as *const () as usize)
}

/// 组件身份：节点地址、共享句柄的 `Rc` 指针、底层值地址。
///
/// TS 的 `removeChild` 用引用相等；Rust 的容器子项是共享句柄代理，调用方既可能传入代理本身，
/// 也可能传入 `&*handle.borrow()` 得到的底层组件，因此三者任一相等即视为同一组件。比较过程
/// 只使用 `try_borrow`（正被外部借用的子项跳过值地址比较），复现 TS 引用比较不会 panic 的语义。
struct ComponentIdentity {
    address: usize,
    rc_ptr: Option<usize>,
    value: Option<usize>,
}

impl ComponentIdentity {
    fn of(node: &dyn Component) -> Self {
        let address = node as *const dyn Component as *const () as usize;
        let inner = node.as_any().and_then(|any| {
            if let Some(handle) = any.downcast_ref::<SharedHandle>() {
                Some(Rc::clone(handle.inner()))
            } else {
                any.downcast_ref::<SharedProxy>()
                    .map(|proxy| Rc::clone(proxy.inner()))
            }
        });
        match inner {
            Some(inner) => ComponentIdentity {
                address,
                rc_ptr: Some(Rc::as_ptr(&inner) as *const () as usize),
                value: shared_value_address_try(&inner),
            },
            None => ComponentIdentity {
                address,
                rc_ptr: None,
                value: Some(address),
            },
        }
    }

    fn matches(&self, node: &dyn Component) -> bool {
        let other = ComponentIdentity::of(node);
        self.address == other.address
            || (self.rc_ptr.is_some() && self.rc_ptr == other.rc_ptr)
            || (self.value.is_some() && self.value == other.value)
    }
}

/// 若 `node` 是共享句柄代理（[`SharedHandle`] / [`SharedProxy`]），返回其底层值地址。
fn component_underlying_address(node: &dyn Component) -> Option<usize> {
    let any = node.as_any()?;
    if let Some(handle) = any.downcast_ref::<SharedHandle>() {
        return Some(shared_value_address(handle.inner()));
    }
    if let Some(proxy) = any.downcast_ref::<SharedProxy>() {
        return Some(shared_value_address(proxy.inner()));
    }
    None
}

/// 在组件树里查找节点（自身地址或底层地址匹配），返回从 `root` 起的下标路径。
///
/// 路径下标按 [`Component::for_each_container_child`] 的遍历顺序编号；根自身匹配时返回空
/// 路径。`target_underlying` 允许把"共享句柄代理"与"它背后的底层组件"视为同一节点。
fn find_component_path(
    root: &dyn Component,
    target_address: usize,
    target_underlying: Option<usize>,
) -> Option<Vec<usize>> {
    let node_address = root as *const dyn Component as *const () as usize;
    if node_address == target_address {
        return Some(Vec::new());
    }
    if let Some(underlying) = component_underlying_address(root)
        && (underlying == target_address || Some(underlying) == target_underlying)
    {
        return Some(Vec::new());
    }
    let mut found = None;
    let mut index = 0usize;
    root.for_each_container_child(&mut |child| {
        if let Some(mut path) = find_component_path(child, target_address, target_underlying) {
            path.insert(0, index);
            found = Some(path);
            false
        } else {
            index += 1;
            true
        }
    });
    found
}

/// 在下标列表（`TuiBase.children` 或隐式子文档的子项）里定位共享组件。
fn focus_target_in_children(
    children: &[Box<dyn Component>],
    value: usize,
    underlying: Option<usize>,
) -> Option<FocusTarget> {
    for (index, child) in children.iter().enumerate() {
        let Some(path) = find_component_path(child.as_ref(), value, underlying) else {
            continue;
        };
        if path.is_empty() {
            return Some(FocusTarget::Component { index });
        }
        let mut indices = Vec::with_capacity(path.len() + 1);
        indices.push(index);
        indices.extend(path);
        return Some(FocusTarget::Path {
            path: focus_path_from(indices),
        });
    }
    None
}

/// 共享组件 → 其值地址与"句柄底层地址"。
fn shared_identity(shared: &SharedComponent) -> (usize, Option<usize>) {
    let value = shared_value_address(shared);
    let underlying = {
        let borrowed = shared.borrow();
        component_underlying_address(&*borrowed)
    };
    (value, underlying)
}

/// 下标列表 → [`FocusPath`]；空列表表示根。`FocusPath::new` 只接受非空列表，这里已单独
/// 处理空列表。
fn focus_path_from(indices: Vec<usize>) -> FocusPath {
    if indices.is_empty() {
        FocusPath::root()
    } else {
        FocusPath::new(&indices).unwrap_or_else(FocusPath::root)
    }
}

// ---------------------------------------------------------------------------
// 生命周期钩子
// ---------------------------------------------------------------------------

/// 退出恢复/停止前置钩子（TS 的 `beforeTerminalStop` / `getExitRestore*` 覆写点）。
///
/// 只能访问共享状态与终端（钩子被 `TuiBase::stop` 在 `&mut self` 借用期间调用，不能再借用
/// 宿主）。`afterTerminalStop` 的文档渲染需要 `&mut self`，由 [`Tui::stop`] 在
/// [`TuiBase::stop`] 返回后执行。
struct AltScreenHooks {
    state: Rc<RefCell<AltState>>,
    terminal: Rc<RefCell<Box<dyn crate::terminal_colors::Terminal>>>,
    mouse_enabled: bool,
}

impl TuiLifecycleHooks for AltScreenHooks {
    fn before_terminal_stop(&mut self, _options: TuiStopOptions) {
        let mut state = self.state.borrow_mut();
        // `closeSearch()`：隐藏 overlay（handle 的 hide 会自行处理焦点/光标）。
        if let Some(mut search) = state.active_search.take()
            && let Some(mut overlay) = search.overlay.take()
        {
            overlay.hide();
        }
        // 停止所有计时器/手势并清空 flash；钩子持有 `&mut AltState`，此时不能再借用宿主，
        // 因此只改共享状态。
        state.stop_selection_auto_scroll();
        state.selection_press_active = false;
        state.stop_scrollbar_hover();
        state.stop_scrollbar_drag();
        state.clear_component_mouse_gesture();
        state.flashes.borrow_mut().dispose();
        if !state.alt_screen_active {
            // 没进过备用屏：不写任何序列（光标/终端由 TuiBase::stop 负责恢复）。
            return;
        }
        // Kitty 图片按 id 删除；这里清掉全部已上传图片。iTerm2/其它协议不需要。
        let deletion = if state.image_protocol == Some(ImageProtocol::Kitty) {
            delete_all_kitty_images()
        } else {
            String::new()
        };
        let mouse = if self.mouse_enabled {
            DISABLE_MOUSE
        } else {
            ""
        };
        // 删除图片、关闭鼠标上报、恢复自动换行在同一个同步输出块（`?2026h` … `?2026l`）里发出；
        // 备用屏本身留给 `afterTerminalStop` / 退出恢复序列。
        self.terminal.borrow_mut().write(&format!(
            "{BEGIN_SYNCHRONIZED_OUTPUT}{deletion}{mouse}{ENABLE_AUTOWRAP}{END_SYNCHRONIZED_OUTPUT}"
        ));
        state.uploaded_kitty_images.clear();
    }

    /// 退出恢复的第一段：关闭同步输出、关闭鼠标上报、恢复自动换行，再拼上 `TuiBase`
    /// 的默认恢复前缀。未进入备用屏时原样返回。
    fn exit_restore_prefix(&mut self, base: &str) -> String {
        let state = self.state.borrow();
        if !state.alt_screen_active {
            return base.to_string();
        }
        let mouse = if self.mouse_enabled {
            DISABLE_MOUSE
        } else {
            ""
        };
        format!("{END_SYNCHRONIZED_OUTPUT}{mouse}{ENABLE_AUTOWRAP}{base}")
    }

    /// 退出恢复的最后一段：离开备用屏并显示光标。未进入备用屏时原样返回。
    fn exit_restore_suffix(&mut self, base: &str) -> String {
        let state = self.state.borrow();
        let suffix = if state.alt_screen_active {
            format!("{EXIT_ALT_SCREEN}\x1b[?25h")
        } else {
            String::new()
        };
        format!("{base}{suffix}")
    }
}

// ---------------------------------------------------------------------------
// TuiAltScreen
// ---------------------------------------------------------------------------

/// 带可滚动、应用自管视口的备用屏 TUI（TS `TuiAltScreen`）。
///
/// 职责边界：
/// - 本类型自己实现备用屏进出、布局帧渲染与差分、鼠标路由（搜索按钮 → overlay → 跟随末尾
///   提示/滚动条 → 布局 → 文本选择）、文本选择与复制、搜索 overlay、Kitty 图片缓存。
/// - 子组件列表、overlay 栈、焦点状态机、输入监听器、终端颜色查询与重绘标志都委托
///   [`TuiBase`]；两者仅在 `stop` 阶段通过 `AltScreenHooks`（`beforeTerminalStop` 与退出恢复
///   前缀/后缀）协作，`start` 的进入序列由 `before_terminal_start_alt` 直接写出。钩子只能访问
///   共享状态，不能在 `TuiBase` 借用期间再借用宿主。
pub struct TuiAltScreen {
    /// overlay/焦点/输入/子组件等共用状态机；本类型只在其上增加视口行为。
    base: TuiBase,
    state: Rc<RefCell<AltState>>,
    /// 隐式子文档（TS `implicitDocument` 的替身）；它是 `implicit_root` 里主滚动视图的被滚
    /// 动子组件，渲染/鼠标派发期间临时持有 `TuiBase.children`。
    document: Rc<RefCell<DocumentComponent>>,
    /// 隐式主滚动视图的状态；没有布局根时，滚动/搜索/选择都退回它。
    implicit_state: Rc<dyn ScrollLayoutState>,
    /// 包住隐式滚动视图的布局句柄；没有布局根时作为布局渲染的根。
    implicit_root: SharedComponent,
    /// `TuiBase.renderRequested` 的共享句柄（内部重绘回调与 [`TuiAltScreen::render_decision`] 共用）。
    render_requested: Rc<Cell<bool>>,
    /// 滚轮行数换算器（含 `Auto` 模式的加速状态）。
    wheel_scroll: WheelScrollAccelerator,
    /// 是否捕获鼠标；构造时由 `options.mouse` 求值一次，运行期不再变化。
    mouse_enabled: bool,
    search_match_style: AltStyleFn,
    search_current_match_style: AltStyleFn,
    search_navigation_button_style: SearchNavigationButtonStyle,
    scroll_to_end_indicator: Option<ScrollToEndIndicatorFn>,
    open_url: Option<OpenUrlFn>,
    on_right_click_paste: Option<RightClickPasteFn>,
    copy_on_select: bool,
    copy_selection: Option<CopySelectionFn>,
    /// 测试钩子：覆盖 `process.platform`（TS 测试用 `Object.defineProperty` 改平台）。
    platform_override: Option<String>,
}

impl TuiAltScreen {
    /// `constructor(terminal, showHardwareCursor?, logDirectory?, options = {})`。
    ///
    /// `show_hardware_cursor` / `log_directory` 原样交给 [`TuiBase::new`]；`options` 的缺省值在
    /// 构造时求值一次，之后修改原选项对象不影响本实例。
    ///
    /// 构造会建立：
    /// - 隐式子文档与包住它的主滚动视图（`follow: End`、`primary: true`）：未调用
    ///   [`ViewportTui::set_layout_root`] 时，布局、滚动、搜索都走这条兜底链路；
    /// - 共享的 `renderRequested` 句柄与 flash 容器：内部组件（滚动视图、flash）请求重绘时
    ///   同时置 `pending_render`（帧内合并）与 `TuiBase.renderRequested`（宿主唤醒）；
    /// - `AltScreenHooks`：在 `TuiBase::stop` 的 `beforeTerminalStop` 阶段写退出清理序列；
    /// - `base.images_capability`：查询 cell size 时读 [`get_capabilities`] 的 `images` 字段。
    ///
    /// 选项缺省值：滚轮固定 1 行、鼠标捕获开启、复制选中开启、搜索高亮使用默认 SGR 样式
    /// （见 [`TuiAltScreenOptions`] 各字段）。构造没有错误返回：内部
    /// [`ScrollView::from_shared`] 使用固定的合法选项（垂直轴），只有内部不变量被破坏时才会
    /// panic。
    pub fn new(
        terminal: Box<dyn crate::terminal_colors::Terminal>,
        show_hardware_cursor: Option<bool>,
        log_directory: Option<String>,
        options: TuiAltScreenOptions,
    ) -> Self {
        let mut base = TuiBase::new(terminal, show_hardware_cursor, log_directory);

        // TS 的 `ScrollView` / `AltScreenFlashContainer` / `renderLayoutFrame` 回调都调用
        // `this.requestRender()`。Rust 的 `'static` 闭包不能借用宿主，因此把
        // `TuiBase.renderRequested` 的共享句柄交给它们，让内部重绘请求真正到达宿主的
        // 渲染循环（`pending_render` 只用于同一帧内的合并）。
        let render_requested = base.render_requested_handle();
        let pending_render = Rc::new(Cell::new(false));
        let internal_request_render: Rc<dyn Fn()> = {
            let pending = Rc::clone(&pending_render);
            let render_requested = Rc::clone(&render_requested);
            Rc::new(move || {
                pending.set(true);
                render_requested.set(true);
            })
        };
        let flashes = Rc::new(RefCell::new(AltScreenFlashContainer::new(Rc::clone(
            &internal_request_render,
        ))));
        let state = {
            let mut state = AltState::new(Rc::clone(&flashes));
            state.pending_render = Rc::clone(&pending_render);
            Rc::new(RefCell::new(state))
        };

        // 隐式文档（TS `implicitDocument`）：`TuiBase.children` 保存真实子组件；渲染与鼠标
        // 派发期间临时把子组件移进本对象（见 [`TuiAltScreen::swap_children_into_document`]）。
        let document: Rc<RefCell<DocumentComponent>> =
            Rc::new(RefCell::new(DocumentComponent::new()));
        let document_shared: SharedComponent = document.clone();

        let implicit_scroll_view = ScrollView::from_shared(
            Rc::clone(&document_shared),
            ScrollViewOptions {
                follow: Some(ScrollViewFollow::End),
                primary: Some(true),
                ..Default::default()
            },
        )
        .expect("ScrollView options are valid");
        implicit_scroll_view.set_request_render(Some(Rc::clone(&internal_request_render)));
        let implicit_state: Rc<dyn ScrollLayoutState> = implicit_scroll_view.state();
        let (implicit_root, _implicit_box) = wrap_child(Box::new(implicit_scroll_view));

        // TS 的 `TuiBase.queryCellSize()` 直接读 `getCapabilities().images`；Rust 骨架把它
        // 做成注入点（`images_capability`）。
        base.images_capability = Some(Box::new(|| get_capabilities().images.is_some()));
        let mouse_enabled = options.mouse.unwrap_or(true);
        base.set_lifecycle_hooks(Box::new(AltScreenHooks {
            state: Rc::clone(&state),
            terminal: Rc::clone(&base.terminal),
            mouse_enabled,
        }));

        let wheel_scroll = match options.wheel_scroll_lines {
            None => WheelScrollAccelerator::new(WheelScrollLines::Fixed(1.0), true),
            Some(WheelScrollLines::Auto) => WheelScrollAccelerator::with_platform_default(),
            Some(fixed) => WheelScrollAccelerator::new(fixed, true),
        };
        let search_match_style: AltStyleFn = options
            .search_match_style
            .unwrap_or_else(|| Rc::new(|text: &str| format!("\x1b[4m{text}\x1b[24m")));
        let search_current_match_style: AltStyleFn = options
            .search_current_match_style
            .unwrap_or_else(|| Rc::new(|text: &str| format!("\x1b[1;7m{text}\x1b[22;27m")));
        let search_navigation_button_style: SearchNavigationButtonStyle = options
            .search_navigation_button_style
            .unwrap_or_else(|| Rc::new(|text: &str, _hovered: bool| text.to_string()));
        let copy_on_select = options.copy_on_select.unwrap_or(true);

        TuiAltScreen {
            base,
            state,
            document,
            implicit_state,
            implicit_root,
            render_requested,
            wheel_scroll,
            mouse_enabled,
            search_match_style,
            search_current_match_style,
            search_navigation_button_style,
            scroll_to_end_indicator: options.scroll_to_end_indicator,
            open_url: options.open_url,
            on_right_click_paste: options.on_right_click_paste,
            copy_on_select,
            copy_selection: options.copy_selection,
            platform_override: None,
        }
    }

    // -- 终端辅助 ---------------------------------------------------------

    fn terminal_columns(&self) -> usize {
        self.base.terminal.borrow().columns()
    }

    fn terminal_rows(&self) -> usize {
        self.base.terminal.borrow().rows()
    }

    fn terminal_write(&self, data: &str) {
        self.base.terminal.borrow_mut().write(data);
    }

    // -- 渲染调度 ---------------------------------------------------------

    /// 宿主渲染循环入口（TS `TuiBase.requestRender()` 的 `nextTick(scheduleRender)`）。
    ///
    /// 宿主只能通过本方法查询调度决策（[`TuiBase`] 由 `TuiAltScreen` 独占）。除了
    /// `TuiBase` 的重绘标志外，这里还统一驱动 TS 里那些 **`setTimeout`/`setInterval` +
    /// `requestRender()`** 的定时器；Rust 侧没有执行器，改由宿主轮询：
    ///
    /// - flash 到期（[`crate::flash::AltScreenFlashContainer`] 的 `setTimeout`）→ 到期渲染；
    /// - 滚动条自动隐藏（`ScrollView` 的 `setTimeout`，见
    ///   [`ScrollLayoutState::poll_timers`]）→ 到期渲染；
    /// - 选择自动滚动（TS 的 `setInterval(50ms)`）→ 每 50 ms 渲染一次。
    ///
    /// 任意一个定时器到期都返回 `Now`；未到期时返回最早的 `Wait`，让宿主在被唤醒后重新查询。
    /// `Wait` 的单位是毫秒（`RenderDecision::Wait(f64)`）。
    ///
    /// 顺序：先取 [`TuiBase::render_decision`]；只有它返回 `Idle` 时才检查 flash、选择自动
    /// 滚动与滚动视图定时器，取最早的 `Wait` 或返回 `Now`，并调用布局树里各滚动视图的
    /// [`ScrollLayoutState::poll_timers`] 结算其到期定时器。`TuiBase` 的 `Wait`/`Now` 优先于
    /// 所有定时器的截止时间，此时本方法完全不会查看定时器。
    ///
    /// 因此，只有走定时器分支时才应把本方法当作"推进定时器"的入口，而不是纯查询。
    pub fn render_decision(&mut self) -> RenderDecision {
        let decision = self.base.render_decision();
        if !matches!(decision, RenderDecision::Idle) {
            return decision;
        }
        let now = Instant::now();
        let mut next_deadline: Option<Instant> = None;
        let mut merge = |deadline: Instant| {
            next_deadline = Some(next_deadline.map_or(deadline, |current| current.min(deadline)));
        };
        if let Some(deadline) = self.state.borrow().flashes.borrow().next_deadline() {
            merge(deadline);
        }
        if let Some(deadline) = self.selection_auto_scroll_deadline() {
            merge(deadline);
        }
        for scroll_view in self.layout_scroll_views() {
            let timers = scroll_view.poll_timers();
            if timers.settled {
                // 定时器刚好到期（TS 的 setTimeout 回调），必须渲染一帧。
                return RenderDecision::Now;
            }
            if let Some(deadline) = timers.next_deadline {
                merge(deadline);
            }
        }
        match next_deadline {
            Some(deadline) if deadline <= now => RenderDecision::Now,
            Some(deadline) => {
                RenderDecision::Wait(deadline.saturating_duration_since(now).as_secs_f64() * 1000.0)
            }
            None => RenderDecision::Idle,
        }
    }

    /// 选择自动滚动的下一个 tick 时刻（TS `setInterval(() => this.autoScrollSelection(), 50)`）。
    fn selection_auto_scroll_deadline(&self) -> Option<Instant> {
        let state = self.state.borrow();
        if state.selection_auto_scroll_direction == 0 {
            return None;
        }
        state
            .selection_auto_scroll_last_tick
            .map(|last| last + Duration::from_millis(AUTO_SCROLL_INTERVAL_MS))
    }

    /// 当前布局树里的全部滚动视图（按出现顺序去重）。
    fn layout_scroll_views(&self) -> Vec<Rc<dyn ScrollLayoutState>> {
        let layout = self.state.borrow().current_layout.clone();
        let Some(layout) = layout else {
            return Vec::new();
        };
        let mut views = Vec::new();
        collect_layout_scroll_views(&layout.root, &mut HashSet::new(), &mut views);
        views
    }

    /// 把 `requestRender` 回调装到布局树里的每个滚动视图上（TS 每次 `updateLayout` 都覆盖）。
    fn install_scroll_request_render(&self, layout: &LayoutFrame, callback: &Rc<dyn Fn()>) {
        let mut views = Vec::new();
        collect_layout_scroll_views(&layout.root, &mut HashSet::new(), &mut views);
        for scroll_view in views {
            scroll_view.set_request_render(Some(Rc::clone(callback)));
        }
    }

    /// 宿主决定渲染时调用（`TuiBase.takeRenderDecisionNow` 的等价物 + flash 到期唤醒）。
    ///
    /// 返回 `true` 表示现在应当调用 [`Tui::render_now`]。
    ///
    /// 与 [`TuiBase::take_render_decision_now`] 的区别：后者只认 `TuiBase` 的重绘标志；这里在
    /// 它为假时还会查询定时器，若为 `Now` 则调用 [`TuiBase::begin_render_now`] 消费一次
    /// （清重绘标志并更新 `lastRenderAt`，维持 16 ms 节流窗口）。
    pub fn take_render_decision_now(&mut self) -> bool {
        if self.base.take_render_decision_now() {
            return true;
        }
        if matches!(self.render_decision(), RenderDecision::Now) {
            self.base.begin_render_now(false);
            return true;
        }
        false
    }

    /// 是否使用隐式子文档渲染（未设置 `layoutRoot` 时）。
    fn uses_implicit_document(&self) -> bool {
        self.state.borrow().layout_root_shared.is_none()
    }

    /// 把 `TuiBase.children` 临时移进隐式子文档（TS 的 `implicitDocument` 直接读 `this.children`）。
    ///
    /// 不变量：隐式子文档只在 `swap…` 与 `restore…` 之间持有子组件；其它时刻
    /// `document.children` 为空，`TuiBase.children` 才是唯一所有者。
    fn swap_children_into_document(&mut self) {
        self.document.borrow_mut().children = std::mem::take(&mut self.base.children);
    }

    /// 渲染/派发结束后把子组件移回 `TuiBase.children`（`Tui::children` 的可见列表）。
    fn restore_children_from_document(&mut self) {
        self.base.children = std::mem::take(&mut self.document.borrow_mut().children);
    }

    /// 在隐式子文档持有子组件期间执行 `f`。
    ///
    /// 没有 panic guard：`f` 若 unwind，恢复语句不会执行，子组件会留在文档里。现有调用
    /// （布局鼠标派发、组件目标重放、`Component::render`）都会执行宿主提供的组件代码，因此
    /// 调用方必须接受这一风险；需要保证恢复时应使用 unwind guard。闭包内的 `return` 只从
    /// 闭包返回，随后仍会执行恢复。
    fn with_document_children<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        self.swap_children_into_document();
        let result = f(self);
        self.restore_children_from_document();
        result
    }

    // -- 公开视口 API -----------------------------------------------------

    /// `viewportTop`：主滚动视图的 `scrollTop`（行）。
    ///
    /// 没有布局帧时读隐式滚动视图（初始为 0）。
    pub fn viewport_top(&self) -> usize {
        self.primary_scroll_state().scroll_top()
    }

    /// `isFollowingOutput`：主滚动视图配置了 `followEnd` 且当前确实停在末尾。
    ///
    /// 没有布局帧时读隐式滚动视图；没有控制面的滚动状态返回 `false`。
    pub fn is_following_output(&self) -> bool {
        self.primary_scroll_state()
            .scroll_view_control()
            .map(ScrollViewControl::is_following_end)
            .unwrap_or(false)
    }

    /// `setWheelScrollLines(lines)`：替换换算规则并重置滚轮加速状态（同 TS `setLines` 的
    /// `reset()`），下一次滚轮事件从"单格一行"重新开始计算速度。
    pub fn set_wheel_scroll_lines(&mut self, lines: WheelScrollLines) {
        self.wheel_scroll.set_lines(lines);
    }

    /// `getCopyOnSelect()`。
    pub fn get_copy_on_select(&self) -> bool {
        self.copy_on_select
    }

    /// 测试钩子：宿主可见的重绘请求标志（`TuiBase.renderRequested` 的共享句柄）。
    ///
    /// 用于断言"一次定时器到期只排一帧"：帧内结算不得把标志重新置起。
    #[doc(hidden)]
    pub fn render_requested_handle(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.render_requested)
    }

    /// `setCopyOnSelect(enabled)`。
    pub fn set_copy_on_select(&mut self, enabled: bool) {
        self.copy_on_select = enabled;
    }

    /// 测试钩子：覆盖平台判定（TS 测试直接改 `process.platform`）。
    #[doc(hidden)]
    pub fn set_platform_override(&mut self, platform: Option<&str>) {
        self.platform_override = platform.map(str::to_string);
    }

    /// 当前平台（测试覆盖优先；缺省映射到 Node 的 `process.platform` 命名）。
    fn platform(&self) -> &str {
        self.platform_override
            .as_deref()
            .unwrap_or(if cfg!(windows) {
                "win32"
            } else {
                std::env::consts::OS
            })
    }

    /// 终端句柄（TS 的公开 `terminal` 字段）。
    pub fn terminal(&self) -> &Rc<RefCell<Box<dyn crate::terminal_colors::Terminal>>> {
        &self.base.terminal
    }

    /// 底层 [`TuiBase`]（overlay/焦点/调试状态；与 [`crate::main_screen::TuiMainScreen::base`]
    /// 同形，供 holder 读取 `onDebug` / `hasOverlayEntries`）。
    pub fn base(&self) -> &TuiBase {
        &self.base
    }

    /// 底层 [`TuiBase`] 的可变视图。
    pub fn base_mut(&mut self) -> &mut TuiBase {
        &mut self.base
    }

    /// `fullRedraws`（全量重绘累计次数）。
    pub fn full_redraws(&self) -> u64 {
        self.base.full_redraws()
    }

    /// `setShowHardwareCursor(enabled)`。
    pub fn set_show_hardware_cursor(&mut self, enabled: bool) {
        self.base.set_show_hardware_cursor(enabled);
    }

    /// `getShowHardwareCursor()`。
    pub fn get_show_hardware_cursor(&self) -> bool {
        self.base.get_show_hardware_cursor()
    }

    /// `setClearOnShrink(enabled)`。
    pub fn set_clear_on_shrink(&mut self, enabled: bool) {
        self.base.set_clear_on_shrink(enabled);
    }

    /// `getClearOnShrink()`。
    pub fn get_clear_on_shrink(&self) -> bool {
        self.base.get_clear_on_shrink()
    }

    /// 取出终端输入队列（不分发）。
    pub fn drain_pending_input(&mut self) -> Vec<String> {
        self.base.drain_pending_input()
    }

    /// `hasActiveSelection()`：选区是否会产生非空文本。
    ///
    /// 等价于 `get_active_selection_text().is_some()`，不触发复制。
    pub fn has_active_selection(&self) -> bool {
        self.get_active_selection_text().is_some()
    }

    /// `copyActiveSelectionToClipboard()`（TS `async`：见 [`CopySelectionStatus`]）。
    pub fn copy_active_selection_to_clipboard(&mut self) -> CopySelectionStatus {
        self.copy_active_selection_to_clipboard_with(None)
    }

    /// `copyActiveSelectionToClipboard()` 的等待版本：复制真正完成时以 `done(ok)` 交付结果
    /// （TS `await copyActiveSelectionToClipboard()` 的返回值）。
    ///
    /// 返回 [`CopySelectionStatus::Pending`] 时 `done` 会在 [`CopySelectionDone`] 到达时调用；
    /// 返回 `Complete` 时 `done` 已在返回前同步调用。
    pub fn copy_active_selection_to_clipboard_with(
        &mut self,
        done: Option<Box<dyn FnOnce(bool)>>,
    ) -> CopySelectionStatus {
        let Some(text) = self.get_active_selection_text() else {
            // TS：`if (!text) return false`（不 flash）。
            if let Some(done) = done {
                done(false);
            }
            return CopySelectionStatus::Complete(false);
        };
        self.copy_text_to_clipboard(&text, done)
    }

    /// `resetTextSelection()`：清空锚点、焦点、粒度、初始区间、按下 URL、拖动/按下状态与
    /// 多击记录。
    ///
    /// 只修改状态，不请求重绘；需要立刻刷新屏幕的调用方要自行调用
    /// [`Tui::request_render`]。
    pub fn reset_text_selection(&mut self) {
        self.state.borrow_mut().clear_text_selection();
        self.state.borrow_mut().last_click = None;
    }

    /// 进程退出兜底恢复（TS `process.on("exit", exitRestoreHandler)`；宿主在收尾时调用）。
    pub fn run_exit_restore_handler(&mut self) {
        self.base.run_exit_restore_handler();
    }

    /// `getScreenLines()`：最后一帧写出的屏幕行。
    ///
    /// 返回的是 `do_render` 处理后的结果（已做伪光标解析、行重置与按终端宽度截断），不是
    /// 组件的原始 `render` 输出；首次渲染前或强制重绘重置后为空。
    pub fn get_screen_lines(&self) -> Vec<String> {
        self.state.borrow().previous_screen.clone()
    }

    /// `setLayoutRoot(component)`。
    ///
    /// 新布局句柄的值地址与当前保存的句柄相同时直接返回，不做重挂载；改变或清空布局根会
    /// 丢弃 `current_layout` 并请求一帧。`None` 表示回到隐式子文档渲染
    /// （`TuiBase.children`）。
    pub fn set_layout_root_component(&mut self, component: Option<Box<dyn Component>>) {
        let previous_root = self.state.borrow().layout_root_shared.clone();
        let previous_value = previous_root.as_ref().map(shared_value_address);
        let (shared, handle) = match component {
            Some(component) => {
                let (layout_shared, handle) = wrap_child(component);
                let next_value = shared_value_address(&layout_shared);
                if previous_value == Some(next_value) {
                    return;
                }
                (Some(layout_shared), Some(handle))
            }
            None => (None, None),
        };
        self.base.set_layout_root(handle);
        {
            let mut state = self.state.borrow_mut();
            state.layout_root_shared = shared;
            state.current_layout = None;
        }
        self.request_render(false);
    }

    /// `flash(message, durationMs?)`：在屏幕顶部显示一条瞬时消息并请求一帧。
    ///
    /// `duration_ms` 为 `None` 时用 flash 容器的默认 1000 ms；到期由
    /// [`TuiAltScreen::render_decision`] 通过 `next_deadline` 发现并驱动渲染，条目在
    /// `render_now` 的 `settle_expired`（或 flash 容器 `render`）中移除。
    pub fn flash(&mut self, message: &str, duration_ms: Option<f64>) {
        self.state
            .borrow()
            .flashes
            .borrow_mut()
            .flash(message, duration_ms);
        self.base.request_render(false);
    }

    /// 以共享句柄添加子组件（TS 里 `addChild(component)` 之后调用方仍持有同一对象）。
    ///
    /// 用 `SharedProxy` 包装：渲染/输入/鼠标/焦点都转发到底层组件，但代理没有独立的
    /// `focused` 镜像（直接写底层组件），因此调用方持有的句柄与 TUI 看到的是同一组件。
    pub fn add_child_shared(&mut self, component: SharedComponent) {
        self.base.add_child(Box::new(SharedProxy::new(component)));
    }

    /// `scrollBy(lines)`：作用于主滚动视图（没有布局帧时为隐式滚动视图）；正数向下。
    ///
    /// 即使当前没有控制面也会请求一帧。
    pub fn scroll_by(&mut self, lines: isize) {
        let scroll_view = self.primary_scroll_state();
        if let Some(control) = scroll_view.scroll_view_control() {
            control.scroll_by(lines as f64);
        }
        self.request_render(false);
    }

    /// `scrollToTop()`：主滚动视图回到顶部并请求一帧。
    pub fn scroll_to_top(&mut self) {
        let scroll_view = self.primary_scroll_state();
        if let Some(control) = scroll_view.scroll_view_control() {
            control.scroll_to_start();
        }
        self.request_render(false);
    }

    /// `scrollToBottom()`：主滚动视图滚到末尾并请求一帧（跟随末尾的滚动视图借此恢复跟随）。
    pub fn scroll_to_bottom(&mut self) {
        let scroll_view = self.primary_scroll_state();
        if let Some(control) = scroll_view.scroll_view_control() {
            control.scroll_to_end();
        }
        self.request_render(false);
    }

    /// 焦点目标解析（TS 的 `setFocus(component)` 在 Rust 侧的入口）。
    ///
    /// 返回该共享组件在挂载树里的 [`FocusTarget`]；调用方再交给 [`Tui::set_focus`]。
    ///
    /// 查找顺序：显式布局根 → `TuiBase.children`。不搜索隐式子文档——该方法用于宿主代码，
    /// 此时子组件在 `TuiBase.children` 里；布局/鼠标派发期间的文档内查找由
    /// `alt_focus_target_of_shared` 负责。组件未挂载时返回 `None`。
    pub fn focus_target_of_component(&self, component: &SharedComponent) -> Option<FocusTarget> {
        let (value, underlying) = shared_identity(component);
        if let Some(root) = self.base.layout_root.as_ref()
            && let Some(path) = find_component_path(root.as_ref(), value, underlying)
        {
            return Some(FocusTarget::LayoutRoot {
                path: focus_path_from(path),
            });
        }
        for (index, child) in self.base.children.iter().enumerate() {
            let Some(path) = find_component_path(child.as_ref(), value, underlying) else {
                continue;
            };
            if path.is_empty() {
                return Some(FocusTarget::Component { index });
            }
            let mut indices = Vec::with_capacity(path.len() + 1);
            indices.push(index);
            indices.extend(path);
            return Some(FocusTarget::Path {
                path: focus_path_from(indices),
            });
        }
        None
    }

    /// `getPrimaryScrollView()`。
    fn primary_scroll_state(&self) -> Rc<dyn ScrollLayoutState> {
        self.state
            .borrow()
            .current_layout
            .as_ref()
            .and_then(|layout| layout.primary_scroll_view.clone())
            .unwrap_or_else(|| Rc::clone(&self.implicit_state))
    }

    /// `deleteKittyImages()`。
    fn delete_kitty_images(&self) -> String {
        if self.state.borrow().image_protocol == Some(ImageProtocol::Kitty) {
            delete_all_kitty_images()
        } else {
            String::new()
        }
    }

    /// `prepareKittyScreen(screen)`：处理一帧里的 Kitty 图片行并维护上传缓存。
    ///
    /// 返回 `(处理后的行, 淘汰图片的删除序列)`：
    /// - 遍历屏幕行，记录可见的图片 id；缓存命中时先移出再插入，使 `IndexMap` 尾部为最近
    ///   使用项（迭代顺序即淘汰顺序）；
    /// - 若该图片的传输代次与缓存相同，用 `placement.replacement_line` 替换整行，否则保留
    ///   原行输出；
    /// - 统计不可见图的条目数/传输字节/估算解码字节，任一超过上限就从最久未使用的不可见图
    ///   开始淘汰，直到全部回到上限内。可见图片永远不会被淘汰。
    fn prepare_kitty_screen(&self, screen: Vec<String>) -> (Vec<String>, String) {
        let mut state = self.state.borrow_mut();
        let mut visible_image_ids: HashSet<u64> = HashSet::new();
        let lines: Vec<String> = screen
            .into_iter()
            .map(|line| {
                let Some(placement) = get_kitty_image_placement(&line) else {
                    return line;
                };
                visible_image_ids.insert(placement.image_id);
                let cached = state
                    .uploaded_kitty_images
                    .get(&placement.image_id)
                    .copied();
                let next = CachedKittyImage {
                    transmission_generation: placement.transmission_generation,
                    transmission_bytes: placement.transmission_bytes,
                    estimated_decoded_bytes: placement.estimated_decoded_bytes,
                };
                // 缓存命中先删除再插入 = 移到 `IndexMap` 末尾（最近使用）。
                if cached.is_some() {
                    state
                        .uploaded_kitty_images
                        .shift_remove(&placement.image_id);
                }
                state.uploaded_kitty_images.insert(placement.image_id, next);
                if cached.map(|cached| cached.transmission_generation)
                    == Some(placement.transmission_generation)
                {
                    placement.replacement_line
                } else {
                    line
                }
            })
            .collect();

        let mut cached_offscreen_image_count = 0usize;
        let mut cached_offscreen_transmission_bytes = 0usize;
        let mut cached_offscreen_decoded_bytes = 0f64;
        for (image_id, cached) in &state.uploaded_kitty_images {
            if visible_image_ids.contains(image_id) {
                continue;
            }
            cached_offscreen_image_count += 1;
            cached_offscreen_transmission_bytes += cached.transmission_bytes;
            cached_offscreen_decoded_bytes += cached.estimated_decoded_bytes;
        }

        // 淘汰：从最久未使用（`IndexMap` 头部）开始，每次迭代都重新检查三个上限；全部达标
        // 即停止。可见图片跳过（但仍继续往后找可淘汰项），计数随淘汰递减。
        let mut evicted_image_deletion = String::new();
        let mut to_remove: Vec<u64> = Vec::new();
        for (image_id, cached) in &state.uploaded_kitty_images {
            if cached_offscreen_image_count <= MAX_CACHED_OFFSCREEN_KITTY_IMAGES
                && cached_offscreen_transmission_bytes
                    <= MAX_CACHED_OFFSCREEN_KITTY_TRANSMISSION_BYTES
                && cached_offscreen_decoded_bytes <= MAX_CACHED_OFFSCREEN_KITTY_DECODED_BYTES
            {
                break;
            }
            if visible_image_ids.contains(image_id) {
                continue;
            }
            evicted_image_deletion.push_str(&delete_kitty_image(*image_id));
            to_remove.push(*image_id);
            cached_offscreen_image_count -= 1;
            cached_offscreen_transmission_bytes -= cached.transmission_bytes;
            cached_offscreen_decoded_bytes -= cached.estimated_decoded_bytes;
        }
        // 迭代期间不能改 `IndexMap`，淘汰列表在循环结束后统一删除。
        for image_id in to_remove {
            state.uploaded_kitty_images.shift_remove(&image_id);
        }
        (lines, evicted_image_deletion)
    }

    /// `resetRenderState()`（override）：丢弃上一帧快照与布局，使下一帧走全量重绘。
    fn reset_render_state_alt(&mut self) {
        let mut state = self.state.borrow_mut();
        state.previous_screen = Vec::new();
        state.previous_screen_width = 0;
        state.previous_screen_height = 0;
        state.current_layout = None;
    }

    /// `beforeTerminalStart()`（顺序见 `spec/tui-core.md` §2.13.4）。
    fn before_terminal_start_alt(&mut self) {
        {
            // 进入备用屏前清掉所有瞬态手势、计时器与 flash 条目，再标记活动状态并快照
            // 当前图片协议。
            let mut state = self.state.borrow_mut();
            state.stop_selection_auto_scroll();
            state.selection_press_active = false;
            state.stop_scrollbar_hover();
            state.stop_scrollbar_drag();
            state.flashes.borrow_mut().dispose();
            state.alt_screen_active = true;
            let capabilities = get_capabilities();
            state.image_protocol = capabilities.images;
            state.uploaded_kitty_images.clear();
            // iTerm2 协议下进入备用屏时临时关闭 images 能力并让所有组件失效；原能力在
            // `after_terminal_stop_alt` 里恢复。
            if capabilities.images == Some(ImageProtocol::Iterm2) {
                state.saved_capabilities = Some(capabilities);
                set_capabilities(TerminalCapabilities {
                    images: None,
                    ..capabilities
                });
                self.base.invalidate_all();
            }
            state.last_document = Vec::new();
            state.selection_anchor = None;
            state.selection_focus = None;
            state.selection_granularity = SelectionGranularity::Character;
            state.selection_initial_range = None;
            state.last_click = None;
            state.pressed_url = None;
            state.selection_dragged = false;
            state.clear_component_mouse_gesture();
            state.last_component_click = None;
        }
        self.reset_render_state_alt();

        // 判定：TMUX/ZELLIJ/STY 环境变量存在，或 TERM（转小写后）以 tmux/screen 开头，
        // 就只上报按键+拖动（`?1002`），否则上报全动作（`?1003`）。
        let term = std::env::var("TERM")
            .map(|value| value.to_lowercase())
            .unwrap_or_default();
        let multiplexer = std::env::var_os("TMUX").is_some()
            || std::env::var_os("ZELLIJ").is_some()
            || std::env::var_os("STY").is_some()
            || term.starts_with("tmux")
            || term.starts_with("screen");
        let mouse_sequence = if multiplexer {
            ENABLE_BUTTON_MOTION_MOUSE
        } else {
            ENABLE_ALL_MOTION_MOUSE
        };
        let mouse = if self.mouse_enabled {
            mouse_sequence
        } else {
            ""
        };
        // 一次写出：进入备用屏 → 关闭自动换行 → 开启鼠标上报（可选）→ 清屏 → 光标归位
        // → 隐藏硬件光标。
        self.terminal_write(&format!(
            "{ENTER_ALT_SCREEN}{DISABLE_AUTOWRAP}{mouse}\x1b[2J\x1b[H\x1b[?25l"
        ));
    }

    /// `afterTerminalStop(options)`（在 `TuiBase::stop` 返回后由 [`Tui::stop`] 调用）。
    ///
    /// `preserve_screen` 为真时只离开备用屏、显示光标；否则把文档重新渲染成普通行写在
    /// 主屏上（退出后仍能看到最后一屏内容），并记录到 `last_document`。
    fn after_terminal_stop_alt(&mut self, options: TuiStopOptions) {
        if !self.state.borrow().alt_screen_active {
            return;
        }
        // 先落标志：表示已退出备用屏，后续基于 `alt_screen_active` 的检查都会走非活动分支。
        self.state.borrow_mut().alt_screen_active = false;
        if options.preserve_screen {
            self.terminal_write(&format!(
                "{BEGIN_SYNCHRONIZED_OUTPUT}{EXIT_ALT_SCREEN}\x1b[?25h{END_SYNCHRONIZED_OUTPUT}"
            ));
        } else {
            // 重新渲染一份"普通行"文档：解析伪光标、去掉光标标记与 OSC 133 前缀、做行重置，
            // 再把非图片行截断到终端宽度（图片行原样保留）。
            let width = self.terminal_columns().max(1);
            let rendered = self.render(width);
            let document_lines: Vec<String> = self
                .base
                .resolve_fake_cursors(rendered)
                .into_iter()
                .map(|line| strip_osc133_zone_prefix(&line))
                .collect();
            let document_lines: Vec<String> = document_lines
                .into_iter()
                .map(|line| line.replace(CURSOR_MARKER, ""))
                .collect();
            let last_document: Vec<String> = self
                .base
                .apply_line_resets(document_lines)
                .into_iter()
                .map(|line| {
                    if is_image_line(&line) || visible_width(&line) <= width {
                        line
                    } else {
                        slice_by_column(&line, 0, width, true)
                    }
                })
                .collect();
            // 主屏输出：离开备用屏并关闭自动换行后，每行先 `\r` 回车 + `\x1b[2K` 清行再写内容，
            // 结尾复位 SGR、恢复自动换行、换行并显示光标。
            let mut buffer =
                format!("{BEGIN_SYNCHRONIZED_OUTPUT}{EXIT_ALT_SCREEN}{DISABLE_AUTOWRAP}");
            for (row, line) in last_document.iter().enumerate() {
                if row > 0 {
                    buffer.push_str("\r\n");
                }
                buffer.push_str(&format!("\r\x1b[2K{line}"));
            }
            buffer.push_str(&format!(
                "\x1b[0m{ENABLE_AUTOWRAP}\r\n\x1b[?25h{END_SYNCHRONIZED_OUTPUT}"
            ));
            self.terminal_write(&buffer);
            self.state.borrow_mut().last_document = last_document;
        }
        // 进入备用屏时被临时关闭的 iTerm2 图片能力在这里恢复。
        let saved = self.state.borrow_mut().saved_capabilities.take();
        if let Some(capabilities) = saved {
            set_capabilities(capabilities);
        }
    }

    // -- 输入 -------------------------------------------------------------

    /// 终端输入入口（TS：alt screen 作为 input listener 先于聚焦组件运行）。
    ///
    /// 顺序（不可交换）：
    /// 1. 先按经过时间推进选择自动滚动（鼠标不动的拖动也继续滚动）；
    /// 2. 视口输入（鼠标/滚轮/搜索/翻页快捷键），被消费则整个输入到此为止；
    /// 3. 其余输入交给 [`TuiBase::handle_terminal_input`]（配色/尺寸回复、输入监听器、
    ///    全局调试键、聚焦组件等）；
    /// 4. 刷新 overlay handle 挂起的焦点标志；
    /// 5. 应用搜索输入组件在步骤 3 里排队的 query。
    pub fn handle_terminal_input(&mut self, data: &str) {
        self.advance_selection_auto_scroll();
        if let Some(result) = self.handle_viewport_input(data)
            && result.consume == Some(true)
        {
            return;
        }
        self.base.handle_terminal_input(data);
        self.base.flush_pending_focus_sync();
        let pending_query = self.state.borrow_mut().pending_query.take();
        if let Some(query) = pending_query {
            self.update_search_query(&query);
        }
    }

    /// 把 `Terminal::start` 回调排队的输入逐条交给 [`Self::handle_terminal_input`]。
    ///
    /// 一次取空队列后按入队顺序分发；分发期间新排队的输入留到下一次调用。
    pub fn pump_pending_input(&mut self) {
        let pending = self.base.drain_pending_input();
        for data in pending {
            self.handle_terminal_input(&data);
        }
    }

    /// `handleViewportInput(data)`（`spec/tui-core.md` §2.13.6，顺序不可变）。
    ///
    /// 返回 `None` 表示未处理（继续交给 `TuiBase` 与组件）；否则返回"消费"结果。
    /// 匹配到快捷键的按键**即使是松开事件**也返回消费（只是不执行动作），避免松开事件
    /// 落到组件里；搜索开关（`toggle`）在 overlay 让位判定之前处理，其它 overlay 聚焦时
    /// 仍可开关搜索，而翻页/滚动类快捷键让位给 overlay。
    fn handle_viewport_input(&mut self, data: &str) -> Option<TuiInputListenerResult> {
        let consume = || {
            Some(TuiInputListenerResult {
                consume: Some(true),
                data: None,
            })
        };

        if data == FOCUS_OUT {
            let had_active_selection = self.state.borrow().selection_press_active;
            let had_non_empty_active_selection =
                had_active_selection && self.get_selection_bounds().is_some();
            {
                let mut state = self.state.borrow_mut();
                state.selection_press_active = false;
                state.stop_selection_auto_scroll();
                state.stop_scrollbar_hover();
            }
            if self.set_search_hovered_direction(None) {
                self.request_render(false);
            }
            {
                let mut state = self.state.borrow_mut();
                state.stop_scrollbar_drag();
                state.pressed_url = None;
                state.selection_dragged = false;
                state.clear_component_mouse_gesture();
                state.last_component_click = None;
                if had_active_selection {
                    state.selection_anchor = None;
                    state.selection_focus = None;
                    state.selection_granularity = SelectionGranularity::Character;
                    state.selection_initial_range = None;
                }
                state.last_click = None;
            }
            if had_active_selection && had_non_empty_active_selection {
                self.request_render(false);
            }
            return consume();
        }
        if data == FOCUS_IN {
            return consume();
        }

        if let Some(wheel_event) = parse_wheel_event(data) {
            let now = crate::tui::monotonic_clock();
            let lines = self.wheel_scroll.next(wheel_event.direction as i32, now);
            // SGR 鼠标按键码用 bit 3（值 8）表示 Alt 修饰键。
            let multiplier = if wheel_event.button & 8 != 0 {
                lines as f64 * ALT_WHEEL_SCROLL_MULTIPLIER
            } else {
                lines as f64
            };
            let wheel_delta = wheel_event.direction as f64 * multiplier;
            let event = self.create_mouse_event(
                TuiMouseEventType::Wheel,
                wheel_event.button,
                wheel_event.x,
                wheel_event.y,
                Some(wheel_delta),
                None,
            );
            let (overlay_hit, overlay_result) = self.dispatch_mouse_to_overlay(&event);
            let result = overlay_result.or(if overlay_hit {
                None
            } else {
                self.dispatch_mouse_to_layout(&event)
            });
            if let Some(result) = result {
                if self.apply_mouse_dispatch_result(&event, &result) {
                    self.request_render(false);
                }
                return consume();
            }
            self.route_wheel(&wheel_event, wheel_delta);
            return consume();
        }
        if let Some(mouse_event) = parse_sgr_mouse_event(data) {
            self.handle_mouse_event(mouse_event);
            return consume();
        }
        if is_mouse_sequence(data) {
            return consume();
        }

        let keybindings = get_keybindings();
        let is_release = is_key_release(data);
        if keybindings.matches(data, "tui.altScreen.search") {
            if !is_release {
                self.toggle_search();
            }
            return consume();
        }
        if self.active_search_overlay_focused() {
            if keybindings.matches(data, "tui.altScreen.searchNext") {
                if !is_release {
                    self.navigate_search(1);
                }
                return consume();
            }
            if keybindings.matches(data, "tui.altScreen.searchPrevious") {
                if !is_release {
                    self.navigate_search(-1);
                }
                return consume();
            }
            if keybindings.matches(data, "tui.altScreen.searchClose") {
                if !is_release {
                    self.close_search();
                }
                return consume();
            }
        }
        if self.should_defer_viewport_input_to_overlay() {
            return None;
        }
        if keybindings.matches(data, "tui.altScreen.pageUp") {
            if !is_release {
                let height = self.primary_scroll_state().viewport_height() as isize;
                self.scroll_by(-height.saturating_sub(PAGE_SCROLL_OVERLAP as isize).max(1));
            }
            return consume();
        }
        if keybindings.matches(data, "tui.altScreen.pageDown") {
            if !is_release {
                let height = self.primary_scroll_state().viewport_height() as isize;
                self.scroll_by(height.saturating_sub(PAGE_SCROLL_OVERLAP as isize).max(1));
            }
            return consume();
        }
        if keybindings.matches(data, "tui.altScreen.halfPageUp") {
            if !is_release {
                let height = self.primary_scroll_state().viewport_height() as isize;
                self.scroll_by(-(height / 2).max(1));
            }
            return consume();
        }
        if keybindings.matches(data, "tui.altScreen.halfPageDown") {
            if !is_release {
                let height = self.primary_scroll_state().viewport_height() as isize;
                self.scroll_by((height / 2).max(1));
            }
            return consume();
        }
        if keybindings.matches(data, "tui.altScreen.lineUp") {
            if !is_release {
                self.scroll_by(-1);
            }
            return consume();
        }
        if keybindings.matches(data, "tui.altScreen.lineDown") {
            if !is_release {
                self.scroll_by(1);
            }
            return consume();
        }
        if keybindings.matches(data, "tui.altScreen.previousPrompt") {
            if !is_release {
                self.scroll_to_prompt(-1);
            }
            return consume();
        }
        if keybindings.matches(data, "tui.altScreen.nextPrompt") {
            if !is_release {
                self.scroll_to_prompt(1);
            }
            return consume();
        }
        if keybindings.matches(data, "tui.altScreen.top") {
            if !is_release {
                self.scroll_to_top();
            }
            return consume();
        }
        if keybindings.matches(data, "tui.altScreen.bottom") {
            if !is_release {
                self.scroll_to_bottom();
            }
            return consume();
        }
        None
    }

    /// `shouldDeferViewportInputToOverlay()`：有 overlay 聚焦且它不是搜索 overlay 时，视口
    /// 快捷键让位给 overlay（搜索快捷键已在此之前处理）。
    fn should_defer_viewport_input_to_overlay(&self) -> bool {
        self.base.is_overlay_focused() && !self.active_search_overlay_focused()
    }

    /// `activeSearch?.overlay?.isFocused()`：没有活动搜索或 overlay 尚未创建时返回 `false`。
    fn active_search_overlay_focused(&self) -> bool {
        let mut state = self.state.borrow_mut();
        let Some(search) = state.active_search.as_mut() else {
            return false;
        };
        let Some(overlay) = search.overlay.as_mut() else {
            return false;
        };
        overlay.is_focused()
    }

    /// `setHoveredNavigationDirection(direction)`（无搜索时返回 false）。
    ///
    /// 返回值 = 搜索组件的 hover 状态是否发生变化；组件类型不符（向下转型失败）按无变化处理。
    fn set_search_hovered_direction(&mut self, direction: Option<i8>) -> bool {
        let mut state = self.state.borrow_mut();
        let Some(search) = state.active_search.as_mut() else {
            return false;
        };
        with_search_component(&search.component, |component| {
            component.set_hovered_navigation_direction(direction)
        })
        .unwrap_or(false)
    }

    /// `decodeMouseButton(button)`：只看低两位（0 左、1 中、2 右，3 表示无按键）。
    fn decode_mouse_button(button: i64) -> TuiMouseButton {
        match button & 3 {
            0 => TuiMouseButton::Left,
            1 => TuiMouseButton::Middle,
            2 => TuiMouseButton::Right,
            _ => TuiMouseButton::None,
        }
    }

    /// `createMouseEvent(type, button, x, y, extra)`。
    ///
    /// `width`/`height` 取终端尺寸（至少 1），`screen_x`/`screen_y` 与 `x`/`y` 相同；
    /// 修饰键来自 SGR 按键码低位：bit 2 = Shift（4）、bit 3 = Alt（8）、bit 4 = Ctrl（16）。
    /// 滚轮事件的 `button` 统一为 `TuiMouseButton::None`，滚动量放在 `wheel_delta`。
    fn create_mouse_event(
        &self,
        event_type: TuiMouseEventType,
        button: i64,
        x: isize,
        y: isize,
        wheel_delta: Option<f64>,
        click_count: Option<u32>,
    ) -> TuiMouseEvent {
        TuiMouseEvent {
            event_type,
            button: if event_type == TuiMouseEventType::Wheel {
                TuiMouseButton::None
            } else {
                Self::decode_mouse_button(button)
            },
            x,
            y,
            screen_x: x,
            screen_y: y,
            width: self.terminal_columns().max(1),
            height: self.terminal_rows().max(1),
            shift: button & 4 != 0,
            alt: button & 8 != 0,
            ctrl: button & 16 != 0,
            wheel_delta,
            click_count,
        }
    }

    /// `dispatchMouseToLayout(event)`。
    fn dispatch_mouse_to_layout(&mut self, event: &TuiMouseEvent) -> Option<DispatchOutcome> {
        if self.uses_implicit_document() {
            // 布局树是隐式滚动视图 → 隐式子文档；派发期间子组件必须位于文档对象里。
            self.with_document_children(|this| this.dispatch_mouse_to_layout_inner(event))
        } else {
            self.dispatch_mouse_to_layout_inner(event)
        }
    }

    fn dispatch_mouse_to_layout_inner(&mut self, event: &TuiMouseEvent) -> Option<DispatchOutcome> {
        let layout = self.state.borrow().current_layout.clone()?;
        // `get_layout_boxes_at` 已按 layer/深度排序（最内层在前）；同一共享组件可能被多个
        // 盒子命中，用 `Rc` 指针去重，只尝试一次。
        let mut visited: Vec<usize> = Vec::new();
        for box_ in get_layout_boxes_at(&layout, event.screen_x, event.screen_y) {
            let component = Rc::clone(&box_.component);
            let key = Rc::as_ptr(&component) as *const () as usize;
            if visited.contains(&key) {
                continue;
            }
            // TS：`getLayoutNode(box.component) && box.component.handleMouse ===
            // Container.prototype.handleMouse` 时跳过。仓库内只有 Stack/ScrollView 提供布局节点，
            // 它们的鼠标处理都是 `Container.handleMouse` 语义，因此这里按"有布局节点"判定。
            if component.borrow().layout_node().is_some() {
                continue;
            }
            visited.push(key);

            // 把事件换算成该盒子的局部坐标；`start_geometry` 记录反向换算所需的信息，
            // 供重放时 `retarget_mouse_event` 重建局部坐标。
            let mut local = event.clone();
            local.x = event.screen_x - box_.rect.x;
            local.y = event.screen_y - box_.rect.y;
            local.width = box_.rect.width;
            local.height = box_.rect.height;
            let start_geometry = TuiMouseDispatchTarget {
                origin_x: box_.rect.x,
                origin_y: box_.rect.y,
                width: box_.rect.width,
                height: box_.rect.height,
            };
            begin_mouse_dispatch_trace();
            let result = dispatch_mouse_event(&mut *component.borrow_mut(), &local);
            let trace = take_mouse_dispatch_trace();
            let Some(result) = result else {
                continue;
            };
            // TS 的 `result.target` 是**最内层**命中的组件（隐式文档、容器都会转发）。
            let (path, geometries, key) =
                target_path_from_trace(shared_value_address(&component), start_geometry, trace);
            let fallback =
                self.alt_focus_target_of_shared(&component)
                    .unwrap_or(FocusTarget::LayoutRoot {
                        path: FocusPath::root(),
                    });
            return Some(DispatchOutcome {
                result,
                target: MouseTarget::Component {
                    component,
                    path,
                    geometries,
                    fallback,
                    key,
                },
            });
        }
        None
    }

    /// 共享组件 → 挂载树里的焦点目标（见模块文档第 5 条）。
    fn alt_focus_target_of_shared(&self, component: &SharedComponent) -> Option<FocusTarget> {
        let (value, underlying) = shared_identity(component);
        if let Some(root) = self.base.layout_root.as_ref()
            && let Some(path) = find_component_path(root.as_ref(), value, underlying)
        {
            return Some(FocusTarget::LayoutRoot {
                path: focus_path_from(path),
            });
        }
        if let Some(target) = focus_target_in_children(&self.base.children, value, underlying) {
            return Some(target);
        }
        // 布局派发期间子组件临时位于隐式子文档里（见 `with_document_children`）。
        let document = self.document.borrow();
        focus_target_in_children(&document.children, value, underlying)
    }

    /// 组件地址 → 挂载树焦点目标（代理感知；`TuiBase::focus_target_of_address` 的补强）。
    ///
    /// `TuiBase` 的实现只比较节点地址，而布局盒 / 容器子项是共享句柄代理：`dispatchMouseEvent`
    /// 记录的是代理**背后**组件的地址，因此按节点地址走会漏掉它们。TS 直接持有组件引用，
    /// 不需要这层解析；这里用 [`find_component_path`]（同时比较句柄底层地址）复刻引用语义。
    fn alt_focus_target_of_address(&self, address: usize) -> Option<FocusTarget> {
        if let Some(root) = self.base.layout_root.as_ref()
            && let Some(path) = find_component_path(root.as_ref(), address, None)
        {
            return Some(FocusTarget::LayoutRoot {
                path: focus_path_from(path),
            });
        }
        for (index, child) in self.base.children.iter().enumerate() {
            let Some(path) = find_component_path(child.as_ref(), address, None) else {
                continue;
            };
            if path.is_empty() {
                return Some(FocusTarget::Component { index });
            }
            let mut indices = Vec::with_capacity(path.len() + 1);
            indices.push(index);
            indices.extend(path);
            return Some(FocusTarget::Path {
                path: focus_path_from(indices),
            });
        }
        // 布局派发期间子组件临时位于隐式子文档里（见 `with_document_children`）。
        let document = self.document.borrow();
        for (index, child) in document.children.iter().enumerate() {
            let Some(path) = find_component_path(child.as_ref(), address, None) else {
                continue;
            };
            if path.is_empty() {
                return Some(FocusTarget::Component { index });
            }
            let mut indices = Vec::with_capacity(path.len() + 1);
            indices.push(index);
            indices.extend(path);
            return Some(FocusTarget::Path {
                path: focus_path_from(indices),
            });
        }
        None
    }

    /// `dispatchMouseToOverlay(event)`。
    ///
    /// 直接派发到 overlay 栈里的组件（而不是经过 `TuiBase` 的冻结
    /// [`crate::tui::TuiMouseDispatchResult`]），以保留组件返回的 `capture`/`render` 字段：
    /// TS 的 `applyMouseDispatchResult` 用它们决定是否记住 mouseCapture、以及是否重绘。
    fn dispatch_mouse_to_overlay(
        &mut self,
        event: &TuiMouseEvent,
    ) -> (bool, Option<DispatchOutcome>) {
        self.base.flush_pending_focus_sync();
        let layouts = self
            .base
            .core()
            .borrow()
            .rendered_overlay_layouts()
            .to_vec();
        // 布局按栈底到栈顶保存，反向遍历 = 最上层优先。指针落在某个 overlay 内就由它接收
        // （即使组件返回 None，也返回 `overlay_hit = true`，事件不再落到下面的布局）。
        for layout in layouts.iter().rev() {
            if event.screen_x < layout.col as isize
                || event.screen_x >= (layout.col + layout.width) as isize
                || event.screen_y < layout.row as isize
                || event.screen_y >= (layout.row + layout.height) as isize
            {
                continue;
            }
            let mut component_event = event.clone();
            component_event.x = event.screen_x - layout.col as isize;
            component_event.y = event.screen_y - layout.row as isize;
            component_event.width = layout.width;
            component_event.height = layout.height;
            let start_geometry = TuiMouseDispatchTarget {
                origin_x: layout.col as isize,
                origin_y: layout.row as isize,
                width: layout.width,
                height: layout.height,
            };
            let (result, start_address, trace) = {
                let core = Rc::clone(self.base.core());
                let mut core = core.borrow_mut();
                let Some(component) = core.overlay_component_mut(layout.id) else {
                    return (true, None);
                };
                let start_address = component as *const dyn Component as *const () as usize;
                begin_mouse_dispatch_trace();
                let result = dispatch_mouse_event(component, &component_event);
                (result, start_address, take_mouse_dispatch_trace())
            };
            let Some(mut result) = result else {
                return (true, None);
            };
            // TS `dispatchMouseToOverlay`：焦点结果交给 overlay 根组件。
            if result.focus == Some(true) {
                result.focus_target = Some(MouseFocusTarget::Explicit(FocusTarget::Overlay {
                    id: layout.id,
                }));
            }
            let (path, geometries, key) =
                target_path_from_trace(start_address, start_geometry, trace);
            return (
                true,
                Some(DispatchOutcome {
                    result,
                    target: MouseTarget::Overlay {
                        id: layout.id,
                        path,
                        geometries,
                        key,
                    },
                }),
            );
        }
        (false, None)
    }

    /// `applyMouseDispatchResult(event, result)`：返回是否需要渲染。
    fn apply_mouse_dispatch_result(
        &mut self,
        event: &TuiMouseEvent,
        outcome: &DispatchOutcome,
    ) -> bool {
        let fallback = match &outcome.target {
            MouseTarget::Component { fallback, .. } => *fallback,
            MouseTarget::Overlay { id, .. } => FocusTarget::Overlay { id: *id },
        };
        // TS 的 `focusTarget` 是组件引用，直接交给 `resolveMouseFocusTarget`。Rust 记录的是
        // 组件地址：先按挂载树（含共享句柄代理）解析成焦点目标，失败才交给 `TuiBase`
        // （`focus_target_of_address` 只比较节点地址，代理后面的组件——例如隐式子文档里的
        // 顶层子项——会被漏掉）。覆盖 overlay 目标时 `Explicit` 原样保留。
        let mut result = outcome.result.clone();
        if result.focus == Some(true)
            && let Some(MouseFocusTarget::Address(address)) = result.focus_target
            && let Some(target) = self.alt_focus_target_of_address(address)
        {
            result.focus_target = Some(MouseFocusTarget::Explicit(target));
        }
        let focus_changed = self.base.apply_mouse_focus_result(&result, fallback);
        // 组件请求 capture：记住这次的目标（含地址链），后续事件直接重放到它，直到松开
        // 清空（`clear_component_mouse_gesture`）。`render` 未指定时，Press/Click/Drag/Wheel
        // 默认需要重绘。
        if result.capture == Some(true) {
            self.state.borrow_mut().mouse_capture = Some(outcome.target.clone());
        }
        let default_render = focus_changed
            || matches!(
                event.event_type,
                TuiMouseEventType::Press
                    | TuiMouseEventType::Click
                    | TuiMouseEventType::Drag
                    | TuiMouseEventType::Wheel
            );
        result.render.unwrap_or(default_render)
    }

    /// `dispatchMouseToTarget(event, target)`。
    ///
    /// TS：`dispatchMouseEvent(target.component, retargetMouseEvent(event, target))`——直接投递给
    /// 最内层目标（不再经过起点容器、不按当前指针位置重新路由）。
    fn dispatch_mouse_to_target(
        &mut self,
        event: &TuiMouseEvent,
        target: &MouseTarget,
    ) -> Option<DispatchOutcome> {
        let result = match target {
            MouseTarget::Component {
                component,
                path,
                geometries,
                ..
            } => {
                // 隐式子文档派发期间子组件被临时移进文档对象（见 `with_document_children`）。
                // 布局树里可派发的盒子都在文档之下，因此重放也必须先把它们移回文档。
                if self.uses_implicit_document() {
                    self.with_document_children(|this| {
                        this.dispatch_component_target(component, path, geometries, event)
                    })
                } else {
                    self.dispatch_component_target(component, path, geometries, event)
                }
            }
            MouseTarget::Overlay {
                id,
                path,
                geometries,
                ..
            } => self.dispatch_mouse_to_overlay_path(*id, path, geometries, event),
        }?;
        Some(DispatchOutcome {
            result,
            target: target.clone(),
        })
    }

    /// 组件起点目标的重放（地址链下钻）。
    fn dispatch_component_target(
        &self,
        component: &SharedComponent,
        path: &[usize],
        geometries: &[TuiMouseDispatchTarget],
        event: &TuiMouseEvent,
    ) -> Option<TuiMouseEventResult> {
        let mut guard = component.borrow_mut();
        dispatch_along_path(&mut *guard, path, geometries, 0, event)
    }

    /// 指定 overlay 的重放（地址链下钻；TS 的 capture 目标重放）。
    fn dispatch_mouse_to_overlay_path(
        &mut self,
        id: usize,
        path: &[usize],
        geometries: &[TuiMouseDispatchTarget],
        event: &TuiMouseEvent,
    ) -> Option<TuiMouseEventResult> {
        self.base.flush_pending_focus_sync();
        let core = Rc::clone(self.base.core());
        let mut core = core.borrow_mut();
        let component = core.overlay_component_mut(id)?;
        dispatch_along_path(component, path, geometries, 0, event)
    }

    /// `getComponentClickCount(target, x, y)`：组件级多击计数。
    ///
    /// 目标身份（最内层接收者地址）、屏幕坐标都相同且间隔不超过 `DOUBLE_CLICK_INTERVAL_MS`
    /// 时按 1→2→3→1 循环，否则重置为 1；每次都记录本次点击。
    fn get_component_click_count(&mut self, target: &MouseTarget, x: isize, y: isize) -> u32 {
        let now = Instant::now();
        let key = target.key();
        let mut state = self.state.borrow_mut();
        let count = match &state.last_component_click {
            Some(previous)
                if now.duration_since(previous.timestamp).as_millis()
                    <= DOUBLE_CLICK_INTERVAL_MS
                    && previous.key == key
                    && previous.x == x
                    && previous.y == y =>
            {
                (previous.count % 3) + 1
            }
            _ => 1,
        };
        state.last_component_click = Some(ComponentClick {
            timestamp: now,
            count,
            key,
            x,
            y,
        });
        count as u32
    }

    /// `handleMouseEvent(raw)`（`spec/tui-core.md` §2.13.8）。
    ///
    /// 事件类型：`m` 结尾 = Release；motion 位（32）为真时，低两位为 3（无按键）是 Move，
    /// 否则是 Drag；其余都是 Press。有 capture 或按下目标时走"重放"分支，否则依次尝试
    /// 搜索按钮、overlay、跟随末尾提示、滚动条、布局组件，最后才轮到右键粘贴与文本选择。
    fn handle_mouse_event(&mut self, raw: SgrMouseEvent) {
        let is_motion = raw.button & 32 != 0;
        let event_type = if raw.release {
            TuiMouseEventType::Release
        } else if is_motion {
            if Self::decode_mouse_button(raw.button) == TuiMouseButton::None {
                TuiMouseEventType::Move
            } else {
                TuiMouseEventType::Drag
            }
        } else {
            TuiMouseEventType::Press
        };
        let event = self.create_mouse_event(event_type, raw.button, raw.x, raw.y, None, None);

        let captured = self.state.borrow().mouse_capture.clone();
        let press_target = self.state.borrow().mouse_press_target.clone();
        // 重放分支：capture 优先于按下目标。松开时若指针从未移动且仍停在按下点，额外合成
        // 一次 Click 并按同一目标重放；无论结果如何都清空组件手势。
        if let Some(target) = captured.or(press_target) {
            {
                let mut state = self.state.borrow_mut();
                if let Some((px, py)) = state.mouse_press_point
                    && (raw.x != px || raw.y != py)
                {
                    state.mouse_press_moved = true;
                    state.last_component_click = None;
                }
            }
            let mut render = false;
            if let Some(result) = self.dispatch_mouse_to_target(&event, &target) {
                render = self.apply_mouse_dispatch_result(&event, &result);
            }
            if raw.release {
                let should_click = {
                    let state = self.state.borrow();
                    !state.mouse_press_moved && state.mouse_press_point == Some((raw.x, raw.y))
                };
                if should_click {
                    let click_count = self.get_component_click_count(&target, raw.x, raw.y);
                    let click_event = self.create_mouse_event(
                        TuiMouseEventType::Click,
                        raw.button,
                        raw.x,
                        raw.y,
                        None,
                        Some(click_count),
                    );
                    if let Some(result) = self.dispatch_mouse_to_target(&click_event, &target)
                        && self.apply_mouse_dispatch_result(&click_event, &result)
                    {
                        render = true;
                    }
                }
                self.state.borrow_mut().clear_component_mouse_gesture();
            }
            if render {
                self.request_render(false);
            }
            return;
        }

        // 未抓取时：搜索 overlay 的导航按钮最先处理（它只认按钮命中，未命中返回 false 继续）。
        if self.handle_search_mouse_event(&raw) {
            return;
        }

        // 其次 overlay（最上层优先）；overlay 未命中才轮到跟随末尾提示与滚动条。
        // overlay 命中时清滚动条 hover；滚动条拖动中不要更新 hover。
        let (overlay_hit, overlay_result) = self.dispatch_mouse_to_overlay(&event);
        if !overlay_hit {
            if self.handle_scroll_to_end_indicator_mouse_event(&raw) {
                return;
            }
            let scrollbar_handled = self.handle_scrollbar_mouse_event(&raw);
            if self.state.borrow().scrollbar_drag.is_none() {
                self.update_scrollbar_hover(raw.x, raw.y);
            }
            if scrollbar_handled {
                return;
            }
        } else {
            self.state.borrow_mut().stop_scrollbar_hover();
        }

        // 最后是布局组件。收到 Press 时清空文本选择并记录按下目标/位置，后续 Drag 与
        // Release（含合成的 Click）都重放到这个目标。
        let result = overlay_result.or(if overlay_hit {
            None
        } else {
            self.dispatch_mouse_to_layout(&event)
        });
        if let Some(result) = result {
            let render = self.apply_mouse_dispatch_result(&event, &result);
            if event_type == TuiMouseEventType::Press {
                self.state.borrow_mut().clear_text_selection();
                let mut state = self.state.borrow_mut();
                state.mouse_press_target = Some(result.target.clone());
                state.mouse_press_point = Some((raw.x, raw.y));
                state.mouse_press_moved = false;
            }
            if render {
                self.request_render(false);
            }
            return;
        }

        // 没有组件接住：先试右键粘贴（仅 win32 且非 VS Code 终端），否则交给文本选择。
        if self.handle_right_click_paste(&raw) {
            return;
        }
        self.handle_selection_mouse_event(&raw);
    }

    /// `routeWheel(event, delta)`。
    ///
    /// 按"最深滚动视图优先"分发滚轮增量（`get_scroll_views_at` 的顺序）：增量被某个视图全部
    /// 消费（剩余为 0）或该视图采用 `Overscroll::Contain` 时停止，否则继续向外层视图传递；
    /// 还有剩余且主滚动视图不在刚才的链里时，把它交给主滚动视图。最后更新滚动条 hover 并
    /// 请求一帧。
    fn route_wheel(&mut self, event: &WheelEvent, delta: f64) {
        let mut remaining = delta;
        let mut seen: Vec<Rc<dyn ScrollLayoutState>> = Vec::new();
        let layout = self.state.borrow().current_layout.clone();
        if let Some(layout) = layout {
            for scroll_view in get_scroll_views_at(&layout, event.x, event.y) {
                seen.push(Rc::clone(&scroll_view));
                if let Some(control) = scroll_view.scroll_view_control() {
                    remaining = control.scroll_by(remaining);
                }
                if remaining == 0.0
                    || scroll_view.overscroll() == crate::layout_node::Overscroll::Contain
                {
                    break;
                }
            }
        }
        let primary = self.primary_scroll_state();
        if remaining != 0.0
            && !seen.iter().any(|seen| Rc::ptr_eq(seen, &primary))
            && let Some(control) = primary.scroll_view_control()
        {
            control.scroll_by(remaining);
        }
        self.update_scrollbar_hover(event.x, event.y);
        self.request_render(false);
    }

    /// `handleRightClickPaste(event)`。
    ///
    /// 只有同时满足：宿主提供了回调、平台是 win32、`TERM_PROGRAM` 不是 vscode、不是松开事件、
    /// 按键码为 2（右键）时才触发；触发时按 best-effort 调用（panic 被吞掉）并返回 `true`。
    fn handle_right_click_paste(&mut self, event: &SgrMouseEvent) -> bool {
        let Some(handler) = self.on_right_click_paste.clone() else {
            return false;
        };
        let term_program = std::env::var("TERM_PROGRAM")
            .map(|value| value.to_lowercase())
            .unwrap_or_default();
        if self.platform() != "win32"
            || term_program == "vscode"
            || event.release
            || event.button != 2
        {
            return false;
        }
        // 剪贴板粘贴是尽力而为（TS `try { this.onRightClickPaste() } catch {}`）。
        run_best_effort(|| handler());
        true
    }

    /// `handleScrollToEndIndicatorMouseEvent(event)`：左键按在上一帧记录的提示矩形内时
    /// 滚到底部。矩形由 `composite_scroll_to_end_indicator` 每帧写入。
    fn handle_scroll_to_end_indicator_mouse_event(&mut self, event: &SgrMouseEvent) -> bool {
        let rect = self.state.borrow().scroll_to_end_indicator_rect;
        let Some(rect) = rect else {
            return false;
        };
        if event.release || event.button & 32 != 0 || event.button & 3 != 0 {
            return false;
        }
        if event.y != rect.row as isize
            || event.x < rect.column
            || event.x >= rect.column + rect.width as isize
        {
            return false;
        }
        self.scroll_to_bottom();
        true
    }

    /// `getScrollbarTargetAt(x, y, includeHiddenAuto)`。
    ///
    /// 有任意 overlay 时禁用滚动条命中；否则按"最深滚动视图优先"找第一个指针正好位于
    /// 滚动条列、且落在轨道纵向范围内的视图。`include_hidden_auto` 决定 `auto` 模式下
    /// 当前隐藏的滚动条是否算命中。
    fn get_scrollbar_target_at(
        &self,
        x: isize,
        y: isize,
        include_hidden_auto: bool,
    ) -> Option<ScrollbarTarget> {
        if self.base.has_overlay() {
            return None;
        }
        let layout = self.state.borrow().current_layout.clone()?;
        for scroll_view in get_scroll_views_at(&layout, x, y) {
            let box_ = get_scroll_view_box(&layout, &scroll_view);
            let geometry = box_.and_then(|box_| {
                if include_hidden_auto {
                    get_scrollbar_geometry_with(box_, true)
                } else {
                    get_scrollbar_geometry(box_)
                }
            });
            if let Some(geometry) = geometry
                && x == geometry.column
                && y >= geometry.track_top
                && y < geometry.track_top + geometry.track_height as isize
            {
                return Some(ScrollbarTarget {
                    scroll_view,
                    geometry,
                });
            }
        }
        None
    }

    /// `updateScrollbarHover(x, y)`。
    fn update_scrollbar_hover(&mut self, x: isize, y: isize) {
        let next = self
            .get_scrollbar_target_at(x, y, true)
            .map(|target| target.scroll_view);
        self.state.borrow_mut().set_scrollbar_hover(next);
    }

    /// `scrollScrollbarToPointer(scrollView, geometry, pointerY, grabOffset)`。
    ///
    /// 拇指可移动距离为 0（内容不超过视口）时直接滚到顶部；否则按"拇指位移 / 可移动距离"
    /// 的比例映射到 `max_scroll_top`，用 `Math.round` 语义取整；`disable_follow` 传 false。
    fn scroll_scrollbar_to_pointer(
        &self,
        scroll_view: &Rc<dyn ScrollLayoutState>,
        geometry: &ScrollbarGeometry,
        pointer_y: isize,
        grab_offset: isize,
    ) {
        let max_thumb_offset = geometry.track_height as isize - geometry.thumb_height as isize;
        let thumb_offset =
            (pointer_y - geometry.track_top - grab_offset).clamp(0, max_thumb_offset.max(0));
        let scroll_top = if max_thumb_offset == 0 {
            0.0
        } else {
            js_round(
                (thumb_offset as f64 / max_thumb_offset as f64) * geometry.max_scroll_top as f64,
            )
        };
        if let Some(control) = scroll_view.scroll_view_control() {
            control.scroll_to(scroll_top, false);
        }
    }

    /// `handleScrollbarMouseEvent(event)`。
    ///
    /// 拖动中消费所有事件（松开结束拖动，其余按当前布局重算几何并让拇指跟随指针）；
    /// 未拖动时只处理左键按下，且命中判定不包含 `auto` 模式下隐藏的滚动条。
    fn handle_scrollbar_mouse_event(&mut self, event: &SgrMouseEvent) -> bool {
        let drag = self.state.borrow().scrollbar_drag.clone();
        if let Some(drag) = drag {
            if event.release {
                self.state.borrow_mut().stop_scrollbar_drag();
                return true;
            }
            let layout = self.state.borrow().current_layout.clone();
            let geometry = layout
                .as_ref()
                .and_then(|layout| get_scroll_view_box(layout, &drag.scroll_view))
                .and_then(get_scrollbar_geometry);
            if let Some(geometry) = geometry {
                self.scroll_scrollbar_to_pointer(
                    &drag.scroll_view,
                    &geometry,
                    event.y,
                    drag.grab_offset,
                );
            }
            return true;
        }

        // 只接受左键按下：松开、motion 或其它按键都不归滚动条。
        if event.release || event.button & 32 != 0 || event.button & 3 != 0 {
            return false;
        }
        let Some(target) = self.get_scrollbar_target_at(event.x, event.y, false) else {
            return false;
        };
        // 开始拖动前清空文本选择与多击记录（滚动条拖动与文本选择互斥）。
        {
            let mut state = self.state.borrow_mut();
            state.stop_selection_auto_scroll();
            state.selection_press_active = false;
            state.selection_anchor = None;
            state.selection_focus = None;
            state.selection_granularity = SelectionGranularity::Character;
            state.selection_initial_range = None;
            state.last_click = None;
            state.pressed_url = None;
            state.selection_dragged = false;
        }
        self.state
            .borrow_mut()
            .set_scrollbar_hover(Some(Rc::clone(&target.scroll_view)));
        // 按在拇指上：抓取偏移 = 按下点相对拇指顶部的距离；按在轨道空白：抓取偏移取拇指
        // 高度的一半（视觉效果是把拇指中心对齐指针），并先把拇指跳过去，再进入拖动。
        let on_thumb = event.y >= target.geometry.thumb_top
            && event.y < target.geometry.thumb_top + target.geometry.thumb_height as isize;
        let grab_offset = if on_thumb {
            event.y - target.geometry.thumb_top
        } else {
            target.geometry.thumb_height as isize / 2
        };
        if !on_thumb {
            self.scroll_scrollbar_to_pointer(
                &target.scroll_view,
                &target.geometry,
                event.y,
                grab_offset,
            );
        }
        self.state.borrow_mut().scrollbar_drag = Some(ScrollbarDrag {
            scroll_view: target.scroll_view,
            grab_offset,
        });
        true
    }

    // -- 搜索 -------------------------------------------------------------

    /// `toggleSearch()`：已有搜索时关闭，否则创建搜索输入组件并在右上角显示 overlay
    /// （宽度 40%、最小 32 列、四周留 1 格边距）。
    ///
    /// 输入组件的查询回调通过 `Weak<RefCell<AltState>>` 只写 `pending_query`：回调发生在
    /// `TuiBase.handle_terminal_input` 期间，此时不能再借用 `TuiAltScreen`；查询会在输入
    /// 分发结束后由 [`TuiAltScreen::handle_terminal_input`] 应用。
    fn toggle_search(&mut self) {
        if self.state.borrow().active_search.is_some() {
            self.close_search();
            return;
        }
        let state_weak: Weak<RefCell<AltState>> = Rc::downgrade(&self.state);
        let on_query_change: Box<dyn FnMut(&str)> = Box::new(move |query: &str| {
            if let Some(state) = state_weak.upgrade() {
                state.borrow_mut().pending_query = Some(query.to_string());
            }
        });
        let component: Rc<RefCell<AltScreenSearchComponent>> =
            Rc::new(RefCell::new(AltScreenSearchComponent::new(
                on_query_change,
                Some(Rc::clone(&self.search_navigation_button_style)),
            )));
        let component_shared: SharedComponent = component.clone();

        let anchor_row = self.primary_scroll_state().scroll_top();
        let mut state = self.state.borrow_mut();
        state.active_search = Some(ActiveSearch {
            component: Rc::clone(&component_shared),
            index: AltScreenSearchIndex::new(),
            overlay: None,
            query: String::new(),
            matches: Rc::new(Vec::new()),
            selected_index: -1,
            selected_key: None,
            anchor_row,
            selection_mode: SearchSelectionMode::Query,
        });
        drop(state);
        let overlay = self.base.show_overlay(
            Box::new(SharedProxy::new(component_shared)),
            OverlayOptions {
                anchor: Some(OverlayAnchor::TopRight),
                width: Some(SizeValue::Percent(40.0)),
                min_width: Some(32.0),
                margin: Some(OverlayMarginValue::All(1.0)),
                ..Default::default()
            },
        );
        if let Some(search) = self.state.borrow_mut().active_search.as_mut() {
            search.overlay = Some(overlay);
        }
    }

    /// `closeSearch()`：丢弃搜索状态、隐藏 overlay 并请求一帧；没有搜索时无操作。
    fn close_search(&mut self) {
        let search = self.state.borrow_mut().active_search.take();
        let Some(mut search) = search else {
            return;
        };
        if let Some(mut overlay) = search.overlay.take() {
            overlay.hide();
        }
        self.request_render(false);
    }

    /// `updateSearchQuery(query)`：记录新查询并重置选择模式；真正的匹配与选中项在下一帧
    /// 的 `refresh_search` 里计算。
    ///
    /// 查询未变化时直接返回。`anchor_row` 取当前选中匹配的首行（若有），否则取主滚动视图
    /// 当前顶部——它决定 `Query` 模式下选中"离视口最近的下一个匹配"。
    fn update_search_query(&mut self, query: &str) {
        let primary_top = self.primary_scroll_state().scroll_top();
        let mut state = self.state.borrow_mut();
        let Some(search) = state.active_search.as_mut() else {
            return;
        };
        if query == search.query {
            return;
        }
        let selected_row = if search.selected_index >= 0 {
            search
                .matches
                .get(search.selected_index as usize)
                .and_then(|match_| match_.segments.first())
                .map(|segment| segment.row)
        } else {
            None
        };
        search.anchor_row = selected_row.unwrap_or(primary_top);
        search.query = query.to_string();
        search.selection_mode = SearchSelectionMode::Query;
        let component = Rc::clone(&search.component);
        with_search_component(&component, |component| component.set_result(-1, 0));
        drop(state);
        self.request_render(false);
    }

    /// `navigateSearch(direction)`：只记录 Next/Previous 模式并请求一帧；实际选中项的
    /// 循环移动在下一帧的 `refresh_search` 里完成（查询为空时无操作）。
    fn navigate_search(&mut self, direction: i8) {
        let mut state = self.state.borrow_mut();
        let Some(search) = state.active_search.as_mut() else {
            return;
        };
        if search.query.is_empty() {
            return;
        }
        search.selection_mode = if direction < 0 {
            SearchSelectionMode::Previous
        } else {
            SearchSelectionMode::Next
        };
        drop(state);
        self.request_render(false);
    }

    /// `getSearchNavigationDirectionAt(x, y)`：指针在搜索 overlay 的当前 bounds 内时，把坐标
    /// 转成 overlay 局部行列交给搜索组件判断命中哪个导航按钮（-1 上一个 / 1 下一个）。
    /// overlay 未渲染（`bounds()` 为 `None`）或指针在外时返回 `None`。
    fn get_search_navigation_direction_at(&mut self, x: isize, y: isize) -> Option<i8> {
        let mut state = self.state.borrow_mut();
        let search = state.active_search.as_mut()?;
        let bounds = search
            .overlay
            .as_mut()
            .and_then(|overlay| overlay.bounds())?;
        if x < bounds.col as isize
            || x >= (bounds.col + bounds.width) as isize
            || y < bounds.row as isize
            || y >= (bounds.row + bounds.height) as isize
        {
            return None;
        }
        let row = y - bounds.row as isize;
        let column = x - bounds.col as isize;
        if row < 0 || column < 0 {
            return None;
        }
        with_search_component(&search.component, |component| {
            component.get_navigation_direction_at(row as usize, column as usize)
        })
        .flatten()
    }

    /// `handleSearchMouseEvent(event)`。
    ///
    /// 先按指针位置更新导航按钮的 hover（变化时请求一帧，即使只是移动），再决定是否导航：
    /// 只有左键按下且命中按钮才返回 `true`（事件被搜索消费）。
    fn handle_search_mouse_event(&mut self, event: &SgrMouseEvent) -> bool {
        if self.state.borrow().active_search.is_none() {
            return false;
        }
        let direction = self.get_search_navigation_direction_at(event.x, event.y);
        if self.set_search_hovered_direction(direction) {
            self.request_render(false);
        }
        if direction.is_none() || event.release || event.button & 32 != 0 || event.button & 3 != 0 {
            return false;
        }
        self.navigate_search(direction.expect("checked above"));
        true
    }

    /// `refreshSearch(layout)` → 是否需要重新布局。
    ///
    /// 每次 `do_render` 在布局之后调用：把主滚动视图的内容行交给搜索索引，按当前
    /// `selection_mode` 决定选中项，写回搜索组件，并在 Query/Next/Previous 模式下把选中的
    /// 匹配滚进视口。
    ///
    /// 模式（`SearchSelectionMode`）：
    /// - `Query`：查询变化后的首次选择，选第一个首行 >= `anchor_row` 的匹配（没有则回绕到
    ///   第一个），即从当前视口附近开始找；
    /// - `Next` / `Previous`：从上次选中项（匹配数组变化时用 key 重新定位）循环移动一格；
    /// - `Retain`：保持/恢复上次选中的匹配（匹配数组变化时按 key 定位，找不到则夹取旧下标）；
    ///   内容与查询都未变时直接返回，不再重算。
    ///
    /// 返回值只表示"选中匹配导致滚动位置变化，需要重新布局一次"；`do_render` 据此重排
    /// 一次（不会循环重排）。
    fn refresh_search(&mut self, layout: &LayoutFrame) -> bool {
        if self.state.borrow().active_search.is_none() {
            return false;
        }
        let scroll_view = layout
            .primary_scroll_view
            .clone()
            .unwrap_or_else(|| Rc::clone(&self.implicit_state));
        let box_ = get_scroll_view_box(layout, &scroll_view);
        // 搜索只索引主滚动视图的内容行；布局里拿不到内容行时清空结果。
        let lines = box_.and_then(|box_| box_.scroll_content_lines.clone());
        let mut state = self.state.borrow_mut();
        let Some(search) = state.active_search.as_mut() else {
            return false;
        };
        let Some(lines) = lines else {
            search.matches = Rc::new(Vec::new());
            search.selected_index = -1;
            search.selected_key = None;
            search.selection_mode = SearchSelectionMode::Retain;
            let component = Rc::clone(&search.component);
            with_search_component(&component, |component| component.set_result(-1, 0));
            return false;
        };
        if trim_js(&search.query).is_empty() {
            search.matches = Rc::new(Vec::new());
            search.selected_index = -1;
            search.selected_key = None;
            search.selection_mode = SearchSelectionMode::Retain;
            let component = Rc::clone(&search.component);
            with_search_component(&component, |component| component.set_result(-1, 0));
            return false;
        }

        // Retain 模式不强制把选中项滚进视口；其余模式在选中项确定后要 reveal。
        let should_reveal_selection = search.selection_mode != SearchSelectionMode::Retain;
        // `changed` = 内容行或规范化 query 变化；匹配数组未变且为 Retain 时无需任何重算。
        let result = search.index.search(&lines, &search.query);
        let matches = Rc::clone(&result.matches);
        search.matches = matches;
        if !result.changed && search.selection_mode == SearchSelectionMode::Retain {
            return false;
        }

        let matches = Rc::clone(&search.matches);
        // 匹配数组变化时用上次选中的 key 在新数组里重新定位（选中同一处匹配）；否则沿用
        // 旧下标。key 由匹配的首尾位置拼成，见 `get_alt_screen_search_match_key`。
        let exact_index: isize = if result.changed {
            match &search.selected_key {
                Some(key) => matches
                    .iter()
                    .position(|match_| &get_alt_screen_search_match_key(match_) == key)
                    .map(|index| index as isize)
                    .unwrap_or(-1),
                None => -1,
            }
        } else {
            search.selected_index
        };
        let mut selected_index: isize = -1;
        if !matches.is_empty() {
            let length = matches.len() as isize;
            // 匹配按行递增（索引按语料顺序生成），因此 Query 模式可以二分：
            // 找第一个"首段行号 >= anchor_row"的匹配；都更早时回绕到第一个。
            match search.selection_mode {
                SearchSelectionMode::Query => {
                    let mut low = 0isize;
                    let mut high = length;
                    while low < high {
                        let middle = low + (high - low) / 2;
                        let row = matches[middle as usize]
                            .segments
                            .first()
                            .map(|segment| segment.row)
                            .unwrap_or(0);
                        if row < search.anchor_row {
                            low = middle + 1;
                        } else {
                            high = middle;
                        }
                    }
                    selected_index = if low < length { low } else { 0 };
                }
                // Next/Previous 在匹配数组上循环移动（越界回绕）；Retain 优先保留 key 命中的
                // 下标，否则把旧下标夹取到 [0, len-1]。
                SearchSelectionMode::Next => {
                    let base = if exact_index >= 0 {
                        exact_index
                    } else {
                        search.selected_index.min(length - 1)
                    };
                    selected_index = if base < 0 { 0 } else { (base + 1) % length };
                }
                SearchSelectionMode::Previous => {
                    let base = if exact_index >= 0 {
                        exact_index
                    } else {
                        search.selected_index.min(length - 1)
                    };
                    selected_index = if base < 0 {
                        length - 1
                    } else {
                        (base - 1 + length) % length
                    };
                }
                SearchSelectionMode::Retain => {
                    selected_index = if exact_index >= 0 {
                        exact_index
                    } else {
                        search.selected_index.max(0).min(length - 1)
                    };
                }
            }
        }

        // 记录选中项并把 "第几个/共几个" 写回搜索组件；之后统一回到 Retain，避免每次渲染
        // 都重复执行 Query/Next/Previous 的选择策略。
        search.selected_index = selected_index;
        search.selected_key = if selected_index >= 0 {
            Some(get_alt_screen_search_match_key(
                &matches[selected_index as usize],
            ))
        } else {
            None
        };
        search.selection_mode = SearchSelectionMode::Retain;
        let component = Rc::clone(&search.component);
        with_search_component(&component, |component| {
            component.set_result(selected_index, matches.len())
        });
        if !should_reveal_selection {
            return false;
        }

        // 把选中匹配滚进视口：完全可见时保持不动；否则把首行放到距顶部 1/3 视口处，
        // 并传 `disable_follow = true`（即使目标恰为内容末尾也不重新启用 follow-end）。
        // 返回值 = 滚动位置是否真的变化（`do_render` 据此决定是否重排一次）。
        let selected = matches.get(selected_index.max(0) as usize);
        let first_segment = selected.and_then(|match_| match_.segments.first());
        let last_segment = selected.and_then(|match_| match_.segments.last());
        let (Some(first_segment), Some(last_segment)) = (first_segment, last_segment) else {
            return false;
        };
        if scroll_view.viewport_height() == 0 {
            return false;
        }
        let before = scroll_view.scroll_top();
        let visible_bottom = before + scroll_view.viewport_height() - 1;
        let mut target = before;
        if first_segment.row < before || last_segment.row > visible_bottom {
            target = first_segment
                .row
                .saturating_sub(scroll_view.viewport_height() / 3);
        }
        if let Some(control) = scroll_view.scroll_view_control() {
            control.scroll_to(target as f64, true);
        }
        scroll_view.scroll_top() != before
    }

    // -- 文本选择 ---------------------------------------------------------

    /// `getScrollSelectionPoint(scrollView, x, y)`：把终端坐标映射成滚动视图内容坐标。
    ///
    /// 指针纵向先夹到"盒子 rect ∩ clip ∩ 终端屏幕"的可见范围，再换算成内容行
    /// （`scrollTop + 相对行`，并夹到内容行数内）；横向夹到盒子宽度 - 1。
    /// 没有布局帧、拿不到盒子或盒子/clip 高度为 0 时返回 `None`。
    fn get_scroll_selection_point(
        &self,
        scroll_view: &Rc<dyn ScrollLayoutState>,
        x: isize,
        y: isize,
    ) -> Option<SelectionPoint> {
        let layout = self.state.borrow().current_layout.clone()?;
        let box_ = get_scroll_view_box(&layout, scroll_view)?;
        if box_.rect.height == 0 || box_.clip.height == 0 {
            return None;
        }
        let rows = self.terminal_rows() as isize;
        let visible_top = 0isize.max(box_.rect.y).max(box_.clip.y);
        let visible_bottom = (rows - 1)
            .min(box_.rect.y + box_.rect.height as isize - 1)
            .min(box_.clip.y + box_.clip.height as isize - 1);
        if visible_bottom < visible_top {
            return None;
        }
        let pointer_row = y.clamp(visible_top, visible_bottom);
        let content_length = box_
            .scroll_content_lines
            .as_ref()
            .map(|lines| lines.len())
            .unwrap_or(1);
        let max_content_row = content_length.saturating_sub(1);
        let row = (scroll_view.scroll_top() as isize + pointer_row - box_.rect.y)
            .clamp(0, max_content_row as isize);
        let col = js_clamp_index(x - box_.rect.x, box_.rect.width as isize - 1);
        Some(SelectionPoint {
            row: row as usize,
            col,
            scroll_view: Some(Rc::clone(scroll_view)),
            boundary: false,
        })
    }

    /// `getSelectionPoint(event, scrollView?)`。
    ///
    /// 给了滚动视图且能映射时用内容坐标（用于跨行选择）；否则退化成终端屏幕坐标
    /// （行/列都夹到终端尺寸 - 1，带 `scroll_view: None` 的普通点）。
    fn get_selection_point(
        &self,
        event: &SgrMouseEvent,
        scroll_view: Option<&Rc<dyn ScrollLayoutState>>,
    ) -> SelectionPoint {
        if let Some(scroll_view) = scroll_view
            && let Some(point) = self.get_scroll_selection_point(scroll_view, event.x, event.y)
        {
            return point;
        }
        let row = js_clamp_index(event.y, self.terminal_rows() as isize - 1);
        let col = js_clamp_index(event.x, self.terminal_columns() as isize - 1);
        SelectionPoint::plain(row, col)
    }

    /// `getSelectionSourceLine(point)`：选择点所在行的**原始**行（含 SGR）。
    ///
    /// 滚动视图内的点优先读该视图盒子的 `scroll_content_lines`（按内容行号），
    /// 读不到时退回上一帧屏幕行；行号越界返回空串。
    fn get_selection_source_line(&self, point: &SelectionPoint) -> String {
        if let Some(scroll_view) = point.scroll_view.as_ref() {
            let layout = self.state.borrow().current_layout.clone();
            if let Some(layout) = layout
                && let Some(box_) = get_scroll_view_box(&layout, scroll_view)
                && let Some(lines) = box_.scroll_content_lines.as_ref()
            {
                return lines.get(point.row).cloned().unwrap_or_default();
            }
        }
        self.state
            .borrow()
            .previous_screen
            .get(point.row)
            .cloned()
            .unwrap_or_default()
    }

    /// `getWordSelection(point)`。
    ///
    /// 把源行按词段切分并累加显示列范围；点击列落在哪一段就选中那段。`selectable` 包含
    /// 普通词段与连接符字符（`TERMINAL_WORD_SELECTION_JOINERS`）；相邻两段都可选且至少一段
    /// 是连接符时合并，因此 `a/b`、`x-y` 这类路径/标识符会被整段选中。点击不在任何段上
    /// （例如空行）返回 `None`。
    fn get_word_selection(&self, point: &SelectionPoint) -> Option<SelectionRange> {
        let line = strip_terminal_sequences(&self.get_selection_source_line(point));
        struct Segment {
            start: usize,
            end: usize,
            selectable: bool,
            joiner: bool,
        }
        let mut segments: Vec<Segment> = Vec::new();
        let mut start = 0usize;
        for segment in get_word_segments(&line) {
            let end = start + visible_width(&segment.segment);
            let joiner = TERMINAL_WORD_SELECTION_JOINERS.contains(&segment.segment.as_str());
            segments.push(Segment {
                start,
                end,
                selectable: segment.is_word_like || joiner,
                joiner,
            });
            start = end;
        }
        let clicked = segments
            .iter()
            .position(|segment| point.col >= segment.start && point.col < segment.end)?;
        let can_join = |left: &Segment, right: &Segment| {
            left.selectable && right.selectable && (left.joiner || right.joiner)
        };
        // 从点击段向两侧扩展，直到不满足连接条件；返回值是排他终点（`boundary: true`）。
        let mut selection_start = segments[clicked].start;
        let mut selection_end = segments[clicked].end;
        let mut index = clicked;
        while index > 0 && can_join(&segments[index - 1], &segments[index]) {
            selection_start = segments[index - 1].start;
            index -= 1;
        }
        let mut index = clicked;
        while index < segments.len() - 1 && can_join(&segments[index], &segments[index + 1]) {
            selection_end = segments[index + 1].end;
            index += 1;
        }
        Some(SelectionRange {
            start: SelectionPoint {
                row: point.row,
                col: selection_start,
                scroll_view: point.scroll_view.clone(),
                boundary: false,
            },
            end: SelectionPoint {
                row: point.row,
                col: selection_end,
                scroll_view: point.scroll_view.clone(),
                boundary: true,
            },
        })
    }

    /// `getLineSelection(point)`：整行选择，区间为 `[0, 可见宽度)`（终点排他）。
    fn get_line_selection(&self, point: &SelectionPoint) -> SelectionRange {
        let width = visible_width(&self.get_selection_source_line(point));
        SelectionRange {
            start: SelectionPoint {
                row: point.row,
                col: 0,
                scroll_view: point.scroll_view.clone(),
                boundary: false,
            },
            end: SelectionPoint {
                row: point.row,
                col: width,
                scroll_view: point.scroll_view.clone(),
                boundary: true,
            },
        }
    }

    /// `updateSelectionFocus(point)`。
    ///
    /// 字符模式直接移动焦点。词/行模式下以 `selection_initial_range` 为基准扩展：目标区间在
    /// 初始区间之前时，锚点取初始区间终点、焦点取目标区间起点；否则锚点取初始起点、焦点取
    /// 目标终点。这样无论向哪侧拖动，选区都覆盖"初始选中的词/行"到"当前词/行"。
    fn update_selection_focus(&mut self, point: &SelectionPoint) {
        let mut state = self.state.borrow_mut();
        if state.selection_granularity == SelectionGranularity::Character
            || state.selection_initial_range.is_none()
        {
            state.selection_focus = Some(point.clone());
            return;
        }
        let granularity = state.selection_granularity;
        let initial = state
            .selection_initial_range
            .clone()
            .expect("checked above");
        drop(state);
        let range = if granularity == SelectionGranularity::Word {
            self.get_word_selection(point)
        } else {
            Some(self.get_line_selection(point))
        };
        let Some(range) = range else {
            return;
        };
        let target_before_initial = range.start.row < initial.start.row
            || (range.start.row == initial.start.row && range.start.col < initial.start.col);
        let mut state = self.state.borrow_mut();
        if target_before_initial {
            state.selection_anchor = Some(initial.end.clone());
            state.selection_focus = Some(range.start);
        } else {
            state.selection_anchor = Some(initial.start.clone());
            state.selection_focus = Some(range.end);
        }
    }

    /// `getClickCount(point, word)`。
    ///
    /// 多击计数：与上次点击同一行、同一滚动视图、同一词边界且间隔不超过
    /// `DOUBLE_CLICK_INTERVAL_MS` 时按 1→2→3→1 循环，否则为 1。`word` 为 `None` 时不计数
    /// 且清空 `last_click`（空行上的点击不会形成多击）。
    fn get_click_count(&mut self, point: &SelectionPoint, word: Option<&SelectionRange>) -> u8 {
        let now = Instant::now();
        let mut state = self.state.borrow_mut();
        let mut count = 1u8;
        if let (Some(word), Some(previous)) = (word, state.last_click.as_ref())
            && now.duration_since(previous.timestamp).as_millis() <= DOUBLE_CLICK_INTERVAL_MS
            && previous.row == point.row
            && same_scroll_view(&previous.scroll_view, &point.scroll_view)
            && previous.word_start == word.start.col
            && previous.word_end == word.end.col
        {
            count = (previous.count % 3) + 1;
        }
        state.last_click = word.map(|word| ClickTarget {
            timestamp: now,
            count,
            row: point.row,
            scroll_view: point.scroll_view.clone(),
            word_start: word.start.col,
            word_end: word.end.col,
        });
        count
    }

    /// `updateSelectionAutoScroll(event)`：根据指针相对滚动视图可见范围的位置设置自动滚动
    /// 方向（在/高于顶部 → -1，在/低于底部 → 1，内部 → 0 并停止），同时记录指针位置；
    /// 第一次进入滚动状态时启动 tick 时钟。缺少锚点/布局/盒子或盒子高度为 0 时停止。
    fn update_selection_auto_scroll(&mut self, event: &SgrMouseEvent) {
        let scroll_view = self
            .state
            .borrow()
            .selection_anchor
            .as_ref()
            .and_then(|anchor| anchor.scroll_view.clone());
        let Some(scroll_view) = scroll_view else {
            self.state.borrow_mut().stop_selection_auto_scroll();
            return;
        };
        let layout = self.state.borrow().current_layout.clone();
        let Some(layout) = layout else {
            self.state.borrow_mut().stop_selection_auto_scroll();
            return;
        };
        let Some(box_) = get_scroll_view_box(&layout, &scroll_view) else {
            self.state.borrow_mut().stop_selection_auto_scroll();
            return;
        };
        if box_.rect.height == 0 || box_.clip.height == 0 {
            self.state.borrow_mut().stop_selection_auto_scroll();
            return;
        }
        let rows = self.terminal_rows() as isize;
        let visible_top = 0isize.max(box_.rect.y).max(box_.clip.y);
        let visible_bottom = (rows - 1)
            .min(box_.rect.y + box_.rect.height as isize - 1)
            .min(box_.clip.y + box_.clip.height as isize - 1);
        {
            let mut state = self.state.borrow_mut();
            state.selection_drag_pointer = Some((event.x, event.y));
            state.selection_auto_scroll_direction = if event.y <= visible_top {
                -1
            } else if event.y >= visible_bottom {
                1
            } else {
                0
            };
            if state.selection_auto_scroll_direction == 0 {
                state.stop_selection_auto_scroll();
                return;
            }
            if state.selection_auto_scroll_last_tick.is_none() {
                state.selection_auto_scroll_last_tick = Some(Instant::now());
            }
        }
    }

    /// 惰性推进选择自动滚动（TS `setInterval(..., 50)` 的等价物）。
    ///
    /// 按"距上次 tick 经过了多少个 50 ms"补算步数（上限 200 步，避免长时间停顿后一次
    /// 循环过多），逐行滚动并重新计算焦点；滚动视图已到边界（`scroll_by` 原样返回方向值）
    /// 或锚点消失时停止。
    fn advance_selection_auto_scroll(&mut self) {
        let (direction, pointer, last_tick) = {
            let state = self.state.borrow();
            (
                state.selection_auto_scroll_direction,
                state.selection_drag_pointer,
                state.selection_auto_scroll_last_tick,
            )
        };
        if direction == 0 {
            return;
        }
        let Some(last_tick) = last_tick else {
            return;
        };
        let elapsed = Instant::now().duration_since(last_tick).as_millis() as u64;
        let steps = (elapsed / AUTO_SCROLL_INTERVAL_MS).min(200) as usize;
        if steps == 0 {
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            state.selection_auto_scroll_last_tick =
                Some(last_tick + Duration::from_millis(AUTO_SCROLL_INTERVAL_MS * steps as u64));
        }
        let Some(pointer) = pointer else {
            self.state.borrow_mut().stop_selection_auto_scroll();
            return;
        };
        for _ in 0..steps {
            let Some(scroll_view) = self
                .state
                .borrow()
                .selection_anchor
                .as_ref()
                .and_then(|anchor| anchor.scroll_view.clone())
            else {
                self.state.borrow_mut().stop_selection_auto_scroll();
                return;
            };
            let remaining = scroll_view
                .scroll_view_control()
                .map(|control| control.scroll_by(direction as f64))
                .unwrap_or(0.0);
            if remaining == direction as f64 {
                self.state.borrow_mut().stop_selection_auto_scroll();
                return;
            }
            if let Some(point) = self.get_scroll_selection_point(&scroll_view, pointer.0, pointer.1)
            {
                self.update_selection_focus(&point);
            }
        }
        self.request_render(false);
    }

    /// `handleSelectionMouseEvent(event)`（`spec/tui-core.md` §2.13.9）。
    ///
    /// 只处理左键（兼容松开事件按键码为 3 的编码）。三个分支：
    /// - 松开：结束本次按下，判断是否算单击（未拖动且落点不变），依次尝试激活 OSC 8 链接、
    ///   把 Click 派发给组件、按 `copyOnSelect` 复制；
    /// - motion（bit 5）：扩展选区并更新自动滚动方向；
    /// - 按下：建立新选区（多击决定字符/词/行粒度），并记录按下点的 OSC 8 URL。
    fn handle_selection_mouse_event(&mut self, event: &SgrMouseEvent) {
        let button = event.button & 3;
        if button != 0 && !(event.release && button == 3) {
            return;
        }
        let anchor_scroll_view = self
            .state
            .borrow()
            .selection_anchor
            .as_ref()
            .and_then(|anchor| anchor.scroll_view.clone());
        let point = self.get_selection_point(event, anchor_scroll_view.as_ref());

        // 松开分支：本次没按下过（例如按下发生在组件上）就直接忽略。
        if event.release {
            if !self.state.borrow().selection_press_active {
                return;
            }
            {
                let mut state = self.state.borrow_mut();
                state.selection_press_active = false;
                state.stop_selection_auto_scroll();
            }
            let anchor = self.state.borrow().selection_anchor.clone();
            let Some(anchor) = anchor else {
                return;
            };
            self.update_selection_focus(&point);
            // 单击判定：从未拖动，且落点（行/列/是否同一滚动视图）与按下点完全一致。
            let is_click = !self.state.borrow().selection_dragged
                && same_scroll_view(&anchor.scroll_view, &point.scroll_view)
                && anchor.row == point.row
                && anchor.col == point.col;
            let clicked_url = if is_click {
                self.state.borrow().pressed_url.clone()
            } else {
                None
            };
            self.state.borrow_mut().pressed_url = None;
            // 单击命中 OSC 8 链接：清空选择后激活链接（best-effort），不再复制或派发点击。
            if let Some(url) = clicked_url
                && let Some(open_url) = self.open_url.clone()
            {
                {
                    let mut state = self.state.borrow_mut();
                    state.selection_anchor = None;
                    state.selection_focus = None;
                }
                // URL 激活是尽力而为（TS `try { this.openUrl(...) } catch {}`）。
                run_best_effort(|| open_url(&url));
                self.request_render(false);
                return;
            }
            // 普通单击：合成 Click 事件先给 overlay/布局组件；任一接收就清空选择并返回，
            // 不再进入复制分支。
            if is_click {
                let click_count = self
                    .state
                    .borrow()
                    .last_click
                    .as_ref()
                    .map(|click| click.count as u32)
                    .unwrap_or(1);
                let click_event = self.create_mouse_event(
                    TuiMouseEventType::Click,
                    event.button,
                    event.x,
                    event.y,
                    None,
                    Some(click_count),
                );
                let (overlay_hit, overlay_result) = self.dispatch_mouse_to_overlay(&click_event);
                let result = overlay_result.or(if overlay_hit {
                    None
                } else {
                    self.dispatch_mouse_to_layout(&click_event)
                });
                if let Some(result) = result {
                    let render = self.apply_mouse_dispatch_result(&click_event, &result);
                    self.state.borrow_mut().clear_text_selection();
                    if render {
                        self.request_render(false);
                    }
                    return;
                }
            }
            // 没有被组件消费的单击：按 `copyOnSelect` 复制选区文本（空选区不复制）。
            if self.copy_on_select {
                let text = self.get_active_selection_text();
                if let Some(text) = text {
                    self.copy_text_to_clipboard(&text, None);
                }
            }
            self.request_render(false);
            return;
        }

        // motion 分支：必须已有激活的按下手势与锚点；标记拖动并清掉多击/URL 记录。
        if event.button & 32 != 0 {
            if !self.state.borrow().selection_press_active
                || self.state.borrow().selection_anchor.is_none()
            {
                return;
            }
            {
                let mut state = self.state.borrow_mut();
                state.selection_dragged = true;
                state.last_click = None;
                state.pressed_url = None;
            }
            self.update_selection_focus(&point);
            self.update_selection_auto_scroll(event);
            self.request_render(false);
            return;
        }

        // 按下分支：开始新的选择手势。没有 overlay 时把锚点绑定到指针下最深的滚动视图
        // （选择内容坐标才能跨行）；双击选中词、三击选中行，其余为字符粒度。
        self.state.borrow_mut().stop_selection_auto_scroll();
        self.state.borrow_mut().selection_press_active = true;
        let scroll_view = {
            let has_overlay = self.base.has_overlay();
            let layout = self.state.borrow().current_layout.clone();
            if !has_overlay && let Some(layout) = layout {
                get_scroll_views_at(&layout, event.x, event.y)
                    .into_iter()
                    .next()
            } else {
                None
            }
        };
        let anchor = self.get_selection_point(event, scroll_view.as_ref());
        let word = self.get_word_selection(&anchor);
        let click_count = self.get_click_count(&anchor, word.as_ref());
        let range = if click_count == 2 {
            word.clone()
        } else if click_count == 3 {
            Some(self.get_line_selection(&anchor))
        } else {
            None
        };
        {
            let mut state = self.state.borrow_mut();
            state.selection_granularity = match (&range, click_count) {
                (Some(_), 2) => SelectionGranularity::Word,
                (Some(_), 3) => SelectionGranularity::Line,
                _ => SelectionGranularity::Character,
            };
            state.selection_initial_range = range.clone();
            state.selection_anchor = Some(
                range
                    .as_ref()
                    .map(|range| range.start.clone())
                    .unwrap_or_else(|| anchor.clone()),
            );
            state.selection_focus = Some(
                range
                    .as_ref()
                    .map(|range| range.end.clone())
                    .unwrap_or_else(|| anchor.clone()),
            );
            state.selection_dragged = false;
        }
        let pressed_url = if range.is_some() {
            None
        } else {
            let row = js_clamp_index(event.y, self.terminal_rows() as isize - 1);
            let col = js_clamp_index(event.x, self.terminal_columns() as isize - 1);
            let line = self
                .state
                .borrow()
                .previous_screen
                .get(row)
                .cloned()
                .unwrap_or_default();
            get_osc8_link_at_column(&line, col)
        };
        self.state.borrow_mut().pressed_url = pressed_url;
        self.request_render(false);
    }

    /// `getSelectionBounds()`：返回按阅读顺序排列的 `(起点, 终点)`。
    ///
    /// 以下情况返回 `None`：没有锚点或焦点、两点属于不同的滚动视图、两点完全相同。
    fn get_selection_bounds(&self) -> Option<(SelectionPoint, SelectionPoint)> {
        let state = self.state.borrow();
        let anchor = state.selection_anchor.as_ref()?;
        let focus = state.selection_focus.as_ref()?;
        if !same_scroll_view(&anchor.scroll_view, &focus.scroll_view) {
            return None;
        }
        let anchor_before_focus =
            anchor.row < focus.row || (anchor.row == focus.row && anchor.col < focus.col);
        if anchor.row == focus.row && anchor.col == focus.col {
            return None;
        }
        Some(if anchor_before_focus {
            (anchor.clone(), focus.clone())
        } else {
            (focus.clone(), anchor.clone())
        })
    }

    /// `getSelectionColumns(line, row, selection, minColumn = 0, maxColumn = width)`。
    ///
    /// 行号/列号保持 TS `number` 的语义（选择整体滚出视口上方时行号可以是负数），调用方按需
    /// 转回 `usize`。`start_*`/`end_*` 是已经加上滚动视图原点的屏幕坐标。
    #[allow(
        clippy::too_many_arguments,
        reason = "逐句对应 TS 的 getSelectionColumns"
    )]
    fn get_selection_columns(
        &self,
        line: &str,
        row: isize,
        start_row: isize,
        start_col: isize,
        end_row: isize,
        end_col: isize,
        end_boundary: bool,
        min_column: isize,
        max_column: isize,
    ) -> (isize, isize) {
        let line_width = visible_width(line) as isize;
        let mut start = min_column;
        let mut end = line_width.min(max_column);
        if row == start_row {
            start = grapheme_start_at(line, start_col).unwrap_or_else(|| start_col.min(line_width));
        }
        if row == end_row {
            end = if end_boundary {
                end_col.min(line_width)
            } else {
                grapheme_end_at(line, end_col).unwrap_or_else(|| (end_col + 1).min(line_width))
            };
        }
        (start.max(min_column), end.min(max_column))
    }

    /// `getActiveSelectionText()`：把选区文本按行取出并用 `\n` 连接。
    ///
    /// 每行按显示列切片，去掉终端序列并做 JS 语义的 `trimEnd`；没有选区或结果为空串时
    /// 返回 `None`。滚动视图选区读该视图的内容行，否则读上一帧屏幕行。
    fn get_active_selection_text(&self) -> Option<String> {
        let selection = self.get_selection_bounds()?;
        let source_lines: Rc<Vec<String>> =
            if let Some(scroll_view) = selection.0.scroll_view.as_ref() {
                let layout = self.state.borrow().current_layout.clone()?;
                let box_ = get_scroll_view_box(&layout, scroll_view)?;
                box_.scroll_content_lines.clone()?
            } else {
                Rc::new(self.state.borrow().previous_screen.clone())
            };
        let mut lines: Vec<String> = Vec::new();
        for row in selection.0.row..=selection.1.row {
            let line = source_lines.get(row).cloned().unwrap_or_default();
            let (start, end) = self.get_selection_columns(
                &line,
                row as isize,
                selection.0.row as isize,
                selection.0.col as isize,
                selection.1.row as isize,
                selection.1.col as isize,
                selection.1.boundary,
                0,
                visible_width(&line) as isize,
            );
            let start = start.max(0) as usize;
            let end = end.max(0) as usize;
            let slice = slice_by_column(&line, start, end.saturating_sub(start), true);
            let text = trim_end_js(&strip_terminal_sequences(&slice)).to_string();
            lines.push(text);
        }
        let text = lines.join("\n");
        if text.is_empty() { None } else { Some(text) }
    }

    /// `copyTextToClipboard(text)`（`spec/tui-core.md` §2.13.9；异步语义见模块文档第 3 条）。
    ///
    /// `done` 是等待最终结果的调用方回调（TS `await` 的返回值）；同步路径返回前调用，
    /// 异步路径在 [`CopySelectionDone`] 到达时调用。
    ///
    /// 结果处理：
    /// - 注入了 `copy_selection` 且返回 `Ready(result)`：立刻 flash 并返回 `Complete(ok)`；
    /// - 返回 `Pending`：不 flash，等 `done(result)` 到达时 flash 并交付 `ok`（只交付一次）；
    /// - 宿主同时调用了 `done` 又返回 `Ready`：`done` 已交付结果，这里不再 flash，按 `Pending`
    ///   返回（避免重复提示）；
    /// - 没有注入回调：走 OSC 52（`\x1b]52;c;<base64>\x07`）并 flash `Copied!`。
    fn copy_text_to_clipboard(
        &mut self,
        text: &str,
        done: Option<Box<dyn FnOnce(bool)>>,
    ) -> CopySelectionStatus {
        let completion: CopyCompletionSlot = Rc::new(RefCell::new(done));
        if let Some(copy_selection) = self.copy_selection.clone() {
            let flashes = Rc::clone(&self.state.borrow().flashes);
            let delivered = Rc::new(Cell::new(false));
            let done_callback: CopySelectionDone = {
                let delivered = Rc::clone(&delivered);
                let completion = Rc::clone(&completion);
                Rc::new(move |result| {
                    // TS 的 Promise 只 resolve 一次。
                    if delivered.replace(true) {
                        return;
                    }
                    let ok = result == CopySelectionResult::Ok;
                    // `await copySelection(text)` 之后的 flash + requestRender。
                    let (message, duration) = copy_selection_flash(&result);
                    flashes.borrow_mut().flash(&message, duration);
                    let callback = completion.borrow_mut().take();
                    if let Some(callback) = callback {
                        callback(ok);
                    }
                })
            };
            match copy_selection(text, done_callback) {
                CopySelectionDispatch::Ready(result) => {
                    if delivered.replace(true) {
                        // 宿主既调用了 done 又返回 Ready：done 已交付结果，不重复 flash，
                        // 状态按 Pending 报（最终 bool 已经通过回调给出）。
                        return CopySelectionStatus::Pending;
                    }
                    let ok = result == CopySelectionResult::Ok;
                    let (message, duration) = copy_selection_flash(&result);
                    self.flash(&message, duration);
                    let callback = completion.borrow_mut().take();
                    if let Some(callback) = callback {
                        callback(ok);
                    }
                    CopySelectionStatus::Complete(ok)
                }
                CopySelectionDispatch::Pending => CopySelectionStatus::Pending,
            }
        } else {
            // 缺省复制路径：OSC 52 把 base64 文本交给终端写系统剪贴板。
            use base64::Engine as _;
            let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
            self.terminal_write(&format!("\x1b]52;c;{encoded}\x07"));
            self.flash("Copied!", None);
            let callback = completion.borrow_mut().take();
            if let Some(callback) = callback {
                callback(true);
            }
            CopySelectionStatus::Complete(true)
        }
    }

    // -- 高亮与合成 -------------------------------------------------------

    /// `applySearchTextHighlight(text, current)`。
    ///
    /// 按 `current` 选择"当前匹配"或"普通匹配"样式函数，只给 ANSI 码之间的纯文本片段套样式，
    /// 原有 SGR 序列逐字保留。
    fn apply_search_text_highlight(&self, text: &str, current: bool) -> String {
        let style = if current {
            &self.search_current_match_style
        } else {
            &self.search_match_style
        };
        let mut result = String::new();
        let mut plain_start = 0usize;
        let mut index = 0usize;
        while index < text.len() {
            let Some(ansi) = extract_ansi_code(text, index) else {
                index += text[index..]
                    .chars()
                    .next()
                    .map(char::len_utf8)
                    .unwrap_or(1);
                continue;
            };
            if index > plain_start {
                result.push_str(&style(&text[plain_start..index]));
            }
            result.push_str(&ansi.code);
            index += ansi.length;
            plain_start = index;
        }
        if plain_start < text.len() {
            result.push_str(&style(&text[plain_start..]));
        }
        result
    }

    /// `applySearchHighlights(screen, layout)`。
    ///
    /// 只处理主滚动视图可见区域内的匹配：先用二分跳过完全在视口上方的匹配，再按内容行
    /// 映射回屏幕行、把每段列范围裁剪到视口与滚动条列，最后逐行从右往左重建高亮片段。
    /// 有选中项（`selected_index >= 0`）才工作，否则原样返回。
    fn apply_search_highlights(&self, screen: Vec<String>, layout: &LayoutFrame) -> Vec<String> {
        let (selected_index, matches) = {
            let state = self.state.borrow();
            let Some(search) = state.active_search.as_ref() else {
                return screen;
            };
            if search.selected_index < 0 || search.matches.is_empty() {
                return screen;
            }
            (search.selected_index as usize, Rc::clone(&search.matches))
        };
        let scroll_view = layout
            .primary_scroll_view
            .clone()
            .unwrap_or_else(|| Rc::clone(&self.implicit_state));
        let Some(box_) = get_scroll_view_box(layout, &scroll_view) else {
            return screen;
        };

        // 可见窗口：屏幕行/列先与布局盒的 rect、clip 取交集；列方向再避开滚动条列。
        // 内容行由屏幕行减去盒子原点再加上 scrollTop 得到。
        let mut ranges_by_row: IndexMap<usize, Vec<SearchHighlightRange>> = IndexMap::new();
        let scrollbar_column = get_scrollbar_geometry(box_).map(|geometry| geometry.column);
        let min_row = 0isize.max(box_.rect.y).max(box_.clip.y);
        let max_row = (screen.len() as isize)
            .min(box_.rect.y + box_.rect.height as isize)
            .min(box_.clip.y + box_.clip.height as isize);
        let min_column = 0isize.max(box_.rect.x).max(box_.clip.x);
        let max_column = (self.terminal_columns() as isize)
            .min(box_.rect.x + box_.rect.width as isize)
            .min(box_.clip.x + box_.clip.width as isize)
            .min(scrollbar_column.unwrap_or(isize::MAX));
        let min_content_row = scroll_view.scroll_top() as isize + min_row - box_.rect.y;
        let max_content_row = scroll_view.scroll_top() as isize + max_row - box_.rect.y - 1;

        // 二分：第一个"最后一段行号 >= 视口首行"的匹配（完全在上方的匹配都跳过）。
        let mut low = 0usize;
        let mut high = matches.len();
        while low < high {
            let middle = low + (high - low) / 2;
            let last_row = matches[middle]
                .segments
                .last()
                .map(|segment| segment.row as isize)
                .unwrap_or(-1);
            if last_row < min_content_row {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        // 匹配按行递增：首行已越过视口底行就停止；每段裁剪到可见行列后按屏幕行收集。
        for (match_index, match_) in matches.iter().enumerate().skip(low) {
            let first_row = match_
                .segments
                .first()
                .map(|segment| segment.row as isize)
                .unwrap_or(0);
            if first_row > max_content_row {
                break;
            }
            for segment in &match_.segments {
                let row = box_.rect.y + segment.row as isize - scroll_view.scroll_top() as isize;
                if row < min_row || row >= max_row {
                    continue;
                }
                let start_col = min_column.max(box_.rect.x + segment.start_col as isize);
                let end_col = max_column.min(box_.rect.x + segment.end_col as isize);
                if end_col <= start_col {
                    continue;
                }
                ranges_by_row
                    .entry(row as usize)
                    .or_default()
                    .push(SearchHighlightRange {
                        start_col: start_col as usize,
                        end_col: end_col as usize,
                        current: match_index == selected_index,
                    });
            }
        }

        // 逐行应用：图片行不动；同一行的多个范围按 `start_col` 降序（从右往左）重建。
        let mut result = screen;
        for (row, mut ranges) in ranges_by_row {
            let Some(line) = result.get(row).cloned() else {
                continue;
            };
            if is_image_line(&line) {
                continue;
            }
            let line_width = visible_width(&line);
            let mut line = line;
            ranges.sort_by_key(|range| std::cmp::Reverse(range.start_col));
            for range in ranges {
                let start_col = range.start_col.min(line_width);
                let end_col = range.end_col.min(line_width);
                if end_col <= start_col {
                    continue;
                }
                let before = slice_by_column(&line, 0, start_col, true);
                let highlighted = slice_by_column(&line, start_col, end_col - start_col, true);
                let after =
                    slice_by_column(&line, end_col, line_width.saturating_sub(end_col), true);
                line = format!(
                    "{before}{}{after}",
                    self.apply_search_text_highlight(&highlighted, range.current)
                );
            }
            result[row] = line;
        }
        result
    }

    /// `applySelectionHighlight(text)`（**【逐字】**）。
    ///
    /// 整段用反色包裹（`\x1b[7m` … `\x1b[27m`）；片段内每个以 `m` 结尾的 SGR 序列之后
    /// 重新补一个 `\x1b[7m`，防止组件自己的 `\x1b[0m`/`\x1b[27m` 取消选中反色。
    fn apply_selection_highlight(&self, text: &str) -> String {
        let mut result = String::from("\x1b[7m");
        let mut index = 0usize;
        while index < text.len() {
            let Some(AnsiCode { code, length }) = extract_ansi_code(text, index) else {
                let ch = text[index..].chars().next().expect("index < len");
                result.push(ch);
                index += ch.len_utf8();
                continue;
            };
            result.push_str(&code);
            if code.ends_with('m') {
                result.push_str("\x1b[7m");
            }
            index += length;
        }
        format!("{result}\x1b[27m")
    }

    /// `applySelection(screen, layout)`。
    ///
    /// 与 TS 一样，映射后的选择行号**保留负值**（选择整体滚到视口上方时 `start.row < 0`），
    /// 可见行只按 `row < start.row || row > end.row` 判定是否高亮；负的起点行不会把高亮
    /// 错误地贴到视口首行上。
    fn apply_selection(&self, screen: Vec<String>, layout: Option<&LayoutFrame>) -> Vec<String> {
        let Some(selection) = self.get_selection_bounds() else {
            return screen;
        };
        let mut start_row = selection.0.row as isize;
        let mut start_col = selection.0.col as isize;
        let mut end_row = selection.1.row as isize;
        let mut end_col = selection.1.col as isize;
        let mut min_row: isize = 0;
        let mut max_row = screen.len() as isize - 1;
        let mut min_column: isize = 0;
        let mut max_column = self.terminal_columns() as isize;
        if let Some(scroll_view) = selection.0.scroll_view.as_ref() {
            let Some(layout) = layout else {
                return screen;
            };
            let Some(box_) = get_scroll_view_box(layout, scroll_view) else {
                return screen;
            };
            min_row = 0isize.max(box_.rect.y).max(box_.clip.y);
            max_row = (screen.len() as isize - 1)
                .min(box_.rect.y + box_.rect.height as isize - 1)
                .min(box_.clip.y + box_.clip.height as isize - 1);
            min_column = 0isize.max(box_.rect.x).max(box_.clip.x);
            max_column = (self.terminal_columns() as isize)
                .min(box_.rect.x + box_.rect.width as isize)
                .min(box_.clip.x + box_.clip.width as isize);
            let scroll_top = scroll_view.scroll_top() as isize;
            start_row = box_.rect.y + selection.0.row as isize - scroll_top;
            start_col = box_.rect.x + selection.0.col as isize;
            end_row = box_.rect.y + selection.1.row as isize - scroll_top;
            end_col = box_.rect.x + selection.1.col as isize;
        }
        screen
            .into_iter()
            .enumerate()
            .map(|(row, line)| {
                let row = row as isize;
                if row < min_row
                    || row > max_row
                    || row < start_row
                    || row > end_row
                    || is_image_line(&line)
                {
                    return line;
                }
                let line_width = visible_width(&line) as isize;
                let (start, end) = self.get_selection_columns(
                    &line,
                    row,
                    start_row,
                    start_col,
                    end_row,
                    end_col,
                    selection.1.boundary,
                    min_column,
                    max_column,
                );
                if end <= start || start < 0 {
                    return line;
                }
                let start = start as usize;
                let end = end as usize;
                let before = slice_by_column(&line, 0, start, true);
                let selected = slice_by_column(&line, start, end - start, true);
                let after =
                    slice_by_column(&line, end, (line_width as usize).saturating_sub(end), true);
                format!(
                    "{before}{}{after}",
                    self.apply_selection_highlight(&selected)
                )
            })
            .collect()
    }

    /// `compositeScrollToEndIndicator(screen, layout, width)`。
    ///
    /// 每帧先清空上一帧的点击矩形；只有主滚动视图配置了 `followEnd` 且当前**不在**末尾、
    /// 且有回调时才绘制。标签居中放在视口最后一行，宽度同时受 clip 与滚动条列限制，
    /// 绘制成功时记录矩形供 `handle_scroll_to_end_indicator_mouse_event` 命中。
    fn composite_scroll_to_end_indicator(
        &mut self,
        screen: Vec<String>,
        layout: &LayoutFrame,
        width: usize,
    ) -> Vec<String> {
        self.state.borrow_mut().scroll_to_end_indicator_rect = None;
        let Some(indicator) = self.scroll_to_end_indicator.clone() else {
            return screen;
        };
        let scroll_view = layout
            .primary_scroll_view
            .clone()
            .unwrap_or_else(|| Rc::clone(&self.implicit_state));
        let follow_end = scroll_view
            .scroll_view_control()
            .map(ScrollViewControl::follow_end)
            .unwrap_or(false);
        let is_following_end = scroll_view
            .scroll_view_control()
            .map(ScrollViewControl::is_following_end)
            .unwrap_or(false);
        if !follow_end || is_following_end {
            return screen;
        }
        let Some(box_) = get_scroll_view_box(layout, &scroll_view) else {
            return screen;
        };
        let clip = box_.clip;
        if clip.width == 0 || clip.height == 0 {
            return screen;
        }
        let row = clip.y + clip.height as isize - 1;
        if row < 0 || row as usize >= screen.len() || is_image_line(&screen[row as usize]) {
            return screen;
        }
        let scrollbar_column = get_scrollbar_geometry(box_).map(|geometry| geometry.column);
        let label = truncate_to_width(&indicator(), clip.width, "", false);
        let label_width = visible_width(&label);
        let column = clip.x + ((clip.width as isize - label_width as isize) / 2);
        let right_edge = scrollbar_column.unwrap_or(clip.x + clip.width as isize);
        let available_width = (right_edge - column).max(0) as usize;
        let text = truncate_to_width(&label, available_width, "", false);
        let text_width = visible_width(&text);
        if text_width == 0 {
            return screen;
        }
        let mut result = screen;
        let base_line = result[row as usize].clone();
        result[row as usize] = composite_tui_line(&base_line, &text, column, text_width, width);
        self.state.borrow_mut().scroll_to_end_indicator_rect = Some(ScrollToEndIndicatorRect {
            row: row as usize,
            column,
            width: text_width,
        });
        result
    }

    /// `compositeFlashes(screen, width, height)`。
    ///
    /// flash 行按底部对齐取最后 `height` 行（多余的上方条目丢弃），屏幕不足 `height` 时
    /// 补空行；每条消息右对齐合成到对应行，宽度为 0 的行跳过。
    fn composite_flashes(&self, screen: Vec<String>, width: usize, height: usize) -> Vec<String> {
        let flash_lines_all = self.state.borrow().flashes.borrow_mut().render(width);
        let flash_lines: Vec<String> = if flash_lines_all.len() > height {
            flash_lines_all[flash_lines_all.len() - height..].to_vec()
        } else {
            flash_lines_all
        };
        if flash_lines.is_empty() {
            return screen;
        }
        let mut result = screen;
        while result.len() < height {
            result.push(String::new());
        }
        for (row, line) in flash_lines.iter().enumerate() {
            let flash_width = visible_width(line);
            if flash_width == 0 {
                continue;
            }
            let base_line = result[row].clone();
            result[row] = composite_tui_line(
                &base_line,
                line,
                width as isize - flash_width as isize,
                flash_width,
                width,
            );
        }
        result
    }

    /// `scrollToPrompt(direction)`：从主滚动视图当前顶部向 `direction` 方向逐行扫描内容，
    /// 遇到第一个 OSC 133 `A`（命令提示开始）标记就把视口滚到该行并请求一帧；扫到内容
    /// 边界仍未找到则什么都不做。
    fn scroll_to_prompt(&mut self, direction: i8) {
        let scroll_view = self.primary_scroll_state();
        let layout = self.state.borrow().current_layout.clone();
        let Some(layout) = layout else {
            return;
        };
        let Some(lines) = get_scroll_view_box(&layout, &scroll_view)
            .and_then(|box_| box_.scroll_content_lines.clone())
        else {
            return;
        };
        let step = direction as isize;
        let mut row = scroll_view.scroll_top() as isize + step;
        while row >= 0 && (row as usize) < lines.len() {
            let line = &lines[row as usize];
            if is_osc133_prompt_start(line) {
                if let Some(control) = scroll_view.scroll_view_control() {
                    control.scroll_to(row as f64, false);
                }
                self.request_render(false);
                return;
            }
            row += step;
        }
    }

    // -- 渲染 -------------------------------------------------------------

    /// `doRender()`（`spec/tui-core.md` §2.13.12，逐句）。
    ///
    /// 渲染路径中唯一真正写终端的地方：布局 → 高亮/合成 → 差分 → 写出 → 记录快照。
    fn do_render(&mut self) {
        // 已停止或未进入备用屏时渲染是空操作；两者任一为真都不写终端。
        if self.base.stopped.get() || !self.state.borrow().alt_screen_active {
            return;
        }
        let width = self.terminal_columns().max(1);
        let height = self.terminal_rows().max(1);

        // 有显式布局根时用它；否则用包住隐式子文档的滚动视图。
        let root = self
            .state
            .borrow()
            .layout_root_shared
            .clone()
            .unwrap_or_else(|| Rc::clone(&self.implicit_root));
        let uses_implicit_document = self.uses_implicit_document();
        let pending = Rc::clone(&self.state.borrow().pending_render);
        // 布局器只借用调用期的 `&dyn Fn()`，而滚动视图需要 `'static` 回调；这里每次渲染
        // 重新构造一个，同时置帧内 pending 与宿主可见的 renderRequested。搜索不借用这个
        // 回调：它通过 TuiBase 的 request_render 请求重绘。
        let request_render: Rc<dyn Fn()> = {
            let pending = Rc::clone(&pending);
            let render_requested = Rc::clone(&self.render_requested);
            Rc::new(move || {
                pending.set(true);
                render_requested.set(true);
            })
        };
        // 隐式文档渲染期间子组件必须位于文档对象里（TS `implicitDocument` 直接读 `this.children`）。
        if uses_implicit_document {
            self.swap_children_into_document();
        }
        let mut next_layout = render_layout_frame(
            Rc::clone(&root),
            width as f64,
            height as f64,
            request_render.as_ref(),
        );
        self.install_scroll_request_render(&next_layout, &request_render);
        // 搜索的选中项可能改变滚动位置：`refresh_search` 返回 true 时只重排一次（不循环）。
        if self.refresh_search(&next_layout) {
            next_layout = render_layout_frame(
                Rc::clone(&root),
                width as f64,
                height as f64,
                request_render.as_ref(),
            );
            self.install_scroll_request_render(&next_layout, &request_render);
        }
        if uses_implicit_document {
            self.restore_children_from_document();
        }

        // 先解析伪光标再高亮/合成：它们只跟踪 SGR。
        let layout_lines: Vec<String> = next_layout
            .lines
            .iter()
            .map(|line| strip_osc133_zone_prefix(line))
            .collect();
        let mut screen = self.base.resolve_fake_cursors(layout_lines);
        screen = self.apply_search_highlights(screen, &next_layout);
        screen = self.composite_scroll_to_end_indicator(screen, &next_layout, width);
        screen = self.base.composite_overlays(screen, width, height);
        // 布局行数可能超过终端高度：只保留最后 `height` 行（底部对齐）。
        if screen.len() > height {
            screen = screen[screen.len() - height..].to_vec();
        }
        screen = self.apply_selection(screen, Some(&next_layout));
        screen = self.composite_flashes(screen, width, height);

        // 光标提取要在行重置/截断之前：它扫描最后 `height` 行里最后一个 `CURSOR_MARKER`，
        // 返回其可见列并删除标记；没有标记时隐藏硬件光标。
        let cursor_pos = self.base.extract_cursor_position(&mut screen, height);
        screen = self
            .base
            .apply_line_resets(screen)
            .into_iter()
            .map(|line| {
                if is_image_line(&line) || visible_width(&line) <= width {
                    line
                } else {
                    slice_by_column(&line, 0, width, true)
                }
            })
            .collect();

        // 取上一帧快照：首帧或尺寸变化 → 全量重绘；`changed_rows` 逐行比较处理后的最终行。
        let (previous_screen, previous_width, previous_height, image_protocol) = {
            let state = self.state.borrow();
            (
                state.previous_screen.clone(),
                state.previous_screen_width,
                state.previous_screen_height,
                state.image_protocol,
            )
        };
        let full_redraw =
            previous_screen.is_empty() || previous_width != width || previous_height != height;
        let changed_rows: Vec<bool> = screen
            .iter()
            .enumerate()
            .map(|(row, line)| Some(line) != previous_screen.get(row))
            .collect();
        // 图片行本身或它原来所在的行发生变化 → 图片锚点需要重画。
        let image_anchors_need_redraw = screen.iter().enumerate().any(|(row, line)| {
            changed_rows[row]
                && (is_image_line(line)
                    || is_image_line(previous_screen.get(row).map(String::as_str).unwrap_or("")))
        });
        let term_program = std::env::var("TERM_PROGRAM")
            .map(|value| value.to_lowercase())
            .unwrap_or_default();
        // TS `Boolean(process.env.WEZTERM_PANE)`：变量存在但为空串时为 false。
        let is_wez_term = std::env::var("WEZTERM_PANE").is_ok_and(|value| !value.is_empty())
            || term_program == "wezterm";
        // WezTerm + Kitty 的额外条件：锚点行没变，但图片放置区覆盖的某行变了，也要重画图片。
        let image_cells_need_redraw = !image_anchors_need_redraw
            && is_wez_term
            && image_protocol == Some(ImageProtocol::Kitty)
            && changed_rows.iter().any(|changed| *changed)
            && screen.iter().enumerate().any(|(row, line)| {
                let Some(placement_rows) = get_kitty_image_placement_rows(line) else {
                    return false;
                };
                (row..row + placement_rows)
                    .any(|covered| changed_rows.get(covered).copied().unwrap_or(false))
            });
        let images_need_redraw = image_anchors_need_redraw || image_cells_need_redraw;
        let redraw_images = full_redraw || images_need_redraw;
        let had_uploaded_kitty_images = !self.state.borrow().uploaded_kitty_images.is_empty();
        // 只有"要重画图片且协议是 Kitty"时才走缓存处理：`prepared_lines` 把传输代次未变的
        // 图片行替换成图片放置给出的 `replacement_line`，`evicted_image_deletion` 是淘汰
        // 图片的删除序列。
        let (prepared_lines, evicted_image_deletion) =
            if redraw_images && image_protocol == Some(ImageProtocol::Kitty) {
                self.prepare_kitty_screen(screen.clone())
            } else {
                (screen.clone(), String::new())
            };

        let mut buffer = String::from(BEGIN_SYNCHRONIZED_OUTPUT);
        // 全量重绘：先删图片再清屏。Kitty 且缓存非空时只删 placements；否则调用
        // `delete_kitty_images()`（Kitty 无缓存时删除全部图片，非 Kitty 协议不产生删除序列，
        // 依赖随后清屏）。局部重绘但图片需要重画时，iTerm2 用清屏、Kitty 删全部 placements。
        if full_redraw {
            self.base.full_redraw_count += 1;
            let clear_images =
                if image_protocol == Some(ImageProtocol::Kitty) && had_uploaded_kitty_images {
                    delete_all_kitty_placements()
                } else {
                    self.delete_kitty_images()
                };
            buffer.push_str(&clear_images);
            buffer.push_str("\x1b[2J");
        } else if images_need_redraw {
            if image_protocol == Some(ImageProtocol::Iterm2) {
                buffer.push_str("\x1b[2J");
            } else if image_protocol == Some(ImageProtocol::Kitty) {
                buffer.push_str(&delete_all_kitty_placements());
            }
        }
        buffer.push_str(&evicted_image_deletion);

        // 某行是否需要重写：非全量、非图片重画、且最终行与上一帧相同则跳过。
        let row_unchanged = |row: usize| {
            !full_redraw && !images_need_redraw && screen.get(row) == previous_screen.get(row)
        };
        // WezTerm + Kitty + 屏幕含图片行：图片分三遍写——先清所有变更行，再写非图片行，
        // 最后写图片行（图片在文本之后定位）。
        let draw_kitty_images_last = redraw_images
            && image_protocol == Some(ImageProtocol::Kitty)
            && screen.iter().any(|line| is_image_line(line))
            && is_wez_term;
        if draw_kitty_images_last {
            // 第一遍：清空所有变更行。
            for row in 0..height {
                if row_unchanged(row) {
                    continue;
                }
                buffer.push_str(&format!("\x1b[{};1H\x1b[2K", row + 1));
            }
            // 第二遍：写非图片行。
            for row in 0..height {
                if row_unchanged(row) {
                    continue;
                }
                if prepared_lines
                    .get(row)
                    .map(|line| is_image_line(line))
                    .unwrap_or(false)
                {
                    continue;
                }
                buffer.push_str(&format!(
                    "\x1b[{};1H{}",
                    row + 1,
                    prepared_lines.get(row).cloned().unwrap_or_default()
                ));
            }
            // 第三遍：写图片行。
            for row in 0..height {
                if row_unchanged(row) {
                    continue;
                }
                if !prepared_lines
                    .get(row)
                    .map(|line| is_image_line(line))
                    .unwrap_or(false)
                {
                    continue;
                }
                buffer.push_str(&format!(
                    "\x1b[{};1H{}",
                    row + 1,
                    prepared_lines.get(row).cloned().unwrap_or_default()
                ));
            }
        } else {
            // 常规路径：逐行定位、清行、写内容；未变化的行跳过。
            for row in 0..height {
                if row_unchanged(row) {
                    continue;
                }
                buffer.push_str(&format!(
                    "\x1b[{};1H\x1b[2K{}",
                    row + 1,
                    prepared_lines.get(row).cloned().unwrap_or_default()
                ));
            }
        }

        // 光标定位：终端行列从 1 开始，列夹到 `width` 内；没有标记时隐藏硬件光标，
        // 有标记时按 `show_hardware_cursor` 决定显示/隐藏（逻辑光标用反色模拟）。
        if let Some((cursor_row, cursor_col)) = cursor_pos {
            buffer.push_str(&format!(
                "\x1b[{};{}H",
                cursor_row + 1,
                width.min(cursor_col) + 1
            ));
            buffer.push_str(if self.base.get_show_hardware_cursor() {
                "\x1b[?25h"
            } else {
                "\x1b[?25l"
            });
        } else {
            buffer.push_str("\x1b[?25l");
        }
        buffer.push_str(END_SYNCHRONIZED_OUTPUT);
        self.terminal_write(&buffer);

        // 写出之后才更新快照：下一次差分以本次实际写出的行为基准。
        {
            let mut state = self.state.borrow_mut();
            state.previous_screen = screen;
            state.previous_screen_width = width;
            state.previous_screen_height = height;
            state.current_layout = Some(next_layout);
        }
        // 渲染期间内部组件可能请求过重绘：清掉帧内标志，合并成一次新的宿主重绘请求，
        // 而不是在本次帧里递归渲染。
        if pending.get() {
            pending.set(false);
            self.base.request_render(false);
        }
    }
}

// ---------------------------------------------------------------------------
// Component / Tui
// ---------------------------------------------------------------------------

impl Component for TuiAltScreen {
    /// 有布局根时只渲染布局根；否则把子组件临时移进隐式子文档，按 `TuiBase.render` 的
    /// 顺序渲染全部子项（TS `super.render(width)` 的等价物）。
    ///
    /// 退出备用屏（`after_terminal_stop_alt`）也调用它取得要写到主屏的文档行。
    fn render(&mut self, width: usize) -> Vec<String> {
        if let Some(root) = self.base.layout_root.as_mut() {
            return root.render(width);
        }
        // TS：`super.render(width)`（Container 渲染全部子组件）。
        self.with_document_children(|this| this.document.borrow_mut().render(width))
    }

    /// 让挂载根（布局根，或没有布局根时的全部子项）与 overlay 栈里的组件失效。
    fn invalidate(&mut self) {
        self.base.invalidate_all();
    }

    /// TS 的 `TuiBase` 没有 `handleInput` 成员（输入走 `handleTerminalInput`）。
    fn handles_input(&self) -> bool {
        false
    }

    /// 把挂载子项列表暴露给 `TuiBase` 的遍历（焦点路径、`containsComponent`、输入装饰）。
    ///
    /// 经 [`Tui::add_child`] / [`TuiAltScreen::add_child_shared`] 挂载的子项是共享句柄代理，
    /// 遍历会透传到真实组件；调用方也可以直接往 [`Tui::children`] 里推入任意组件。
    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        Some(&self.base.children)
    }

    /// [`Component::container_children`] 的可变版本。
    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        Some(&mut self.base.children)
    }

    /// 向下转型：`Tui` trait 不暴露宿主终端，扩展 UI 层用它取回 [`TuiAltScreen::terminal`]
    /// （`tui.terminal`）。
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    /// 向下转型的可变版本（扩展层持有 `&mut dyn Tui` 时取回 [`TuiAltScreen`]）。
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl Tui for TuiAltScreen {
    /// `TuiMode::Fullscreen`：备用屏渲染器。
    fn mode(&self) -> TuiMode {
        TuiMode::Fullscreen
    }

    /// `true`：宿主据此使用 [`ViewportTui`] 接口（`set_layout_root`）。
    fn is_viewport_tui(&self) -> bool {
        true
    }

    /// TS `TuiBase.children`（`Container.children` 的公开数组）。
    ///
    /// 子组件以共享句柄代理的形式保存（Rust 的 `Box<dyn Component>` 不能同时被隐式子文档与
    /// `TuiBase` 持有），代理把渲染/输入/焦点全部转发给同一底层组件，因此对列表的读取、
    /// [`Tui::add_child`]、[`Tui::remove_child`] 与 [`Tui::clear_children`] 都作用于同一批组件。
    fn children(&mut self) -> &mut Vec<Box<dyn Component>> {
        &mut self.base.children
    }

    /// 把子组件包成共享句柄后挂载；与 [`TuiAltScreen::add_child_shared`] 等价，区别是
    /// 调用方拿不到返回的句柄。
    fn add_child(&mut self, child: Box<dyn Component>) {
        let (shared, _handle) = wrap_child(child);
        self.add_child_shared(shared);
    }

    /// 按下标移除（[`Tui::remove_child_at`]）：同步子项 id 与焦点状态（共享句柄随
    /// 引用计数释放）。
    fn remove_child_at(&mut self, index: usize) -> Option<Box<dyn Component>> {
        self.base.remove_child_at(index)
    }

    /// `Container.removeChild(component)`：只按引用（身份）移除，**不动焦点**。
    ///
    /// TS 的 `focusedComponent` 是组件引用：删除聚焦组件后它继续接收输入（真正的不动焦点）。
    /// Rust 侧由 [`TuiBase::remove_child_at`] 把该子树保留为"脱离焦点"来复现。
    fn remove_child(&mut self, child: &dyn Component) {
        let target = ComponentIdentity::of(child);
        if let Some(index) = self
            .base
            .children
            .iter()
            .position(|candidate| target.matches(candidate.as_ref()))
        {
            self.base.remove_child_at(index);
        }
    }

    /// `Container.clear()`：清空子组件，**不动焦点**（聚焦子树由 `TuiBase` 保留，见上）。
    fn clear_children(&mut self) {
        self.base.clear_children();
    }

    /// 当前焦点目标（由 `TuiBase` 的焦点状态机维护）。
    fn focus(&self) -> Option<FocusTarget> {
        self.base.focused_component()
    }

    /// 设置焦点；`TuiBase` 会同步焦点标志到真实组件，并记住新地址。
    fn set_focus(&mut self, target: Option<FocusTarget>) {
        self.base.set_focus(target);
    }

    /// 显示 overlay：`options.non_capturing` 为假**且** overlay 在插入时可见，才自动把焦点
    /// 交给 overlay 根；最后请求一帧。
    fn show_overlay(
        &mut self,
        component: Box<dyn Component>,
        options: OverlayOptions,
    ) -> Box<dyn OverlayHandle> {
        self.base.show_overlay(component, options)
    }

    /// 隐藏最上层 overlay；若它持有焦点，则交给最上层可见 overlay 或它的 preFocus，
    /// 栈空时隐藏终端光标（由 [`TuiBase::hide_overlay`] 完成）。
    fn hide_overlay(&mut self) {
        self.base.hide_overlay();
    }

    /// 是否有可见 overlay。
    fn has_overlay(&self) -> bool {
        self.base.has_overlay()
    }

    /// `start()`：先把 `stopped` 置 false（TS 顺序：在 `beforeTerminalStart` 之前），
    /// 再写进入备用屏的序列，最后让 `TuiBase.start` 启动终端、标记退出恢复已注册并请求一帧
    /// （Rust 侧没有 `process.on("exit")`，宿主需要自己调用 `run_exit_restore_handler`）。
    fn start(&mut self) {
        // TS：`stopped = false` 在 beforeTerminalStart 之前。
        self.base.stopped.set(false);
        self.before_terminal_start_alt();
        self.base.start();
    }

    /// `stop(options)`：先走 `TuiBase.stop`（触发 `beforeTerminalStop` 清理、显示光标、停终端），
    /// 返回后再处理备用屏退出与主屏文档输出（见 `after_terminal_stop_alt`）。
    fn stop(&mut self, options: TuiStopOptions) {
        self.base.stop(options);
        self.after_terminal_stop_alt(options);
    }

    /// 渲染一帧：先结算三类惰性定时器，再清重绘标志并调用 `do_render`。
    ///
    /// `force` 为真时先丢弃上一帧快照（下一帧全量重绘）。
    fn render_now(&mut self, force: bool) {
        if force {
            self.reset_render_state_alt();
        }
        // TS 里这些状态变化来自**独立触发**的定时器回调（`setInterval(50ms)` 的选择自动滚动、
        // flash / 滚动条自动隐藏的 `setTimeout`），回调先改状态并 `requestRender()`，随后的
        // 调度帧再渲染。这里在清掉重绘标志**之前**结算：本次帧已经包含变化，重绘请求不应
        // 再排第二帧（否则每次到期多写一次空帧，见 `spec/tui-core.md` §2.13.12）。
        self.advance_selection_auto_scroll();
        self.state.borrow().flashes.borrow_mut().settle_expired();
        for scroll_view in self.layout_scroll_views() {
            scroll_view.poll_timers();
        }
        self.base.begin_render_now(false);
        self.state.borrow().pending_render.set(false);
        self.do_render();
    }

    /// 请求重绘：清掉帧内合并标志（上一帧的遗留请求不应影响新调度），`force` 时重置快照，
    /// 再交给 `TuiBase.request_render`（`force = true` 会走立即渲染）。
    fn request_render(&mut self, force: bool) {
        self.state.borrow().pending_render.set(false);
        if force {
            self.reset_render_state_alt();
        }
        self.base.request_render(force);
    }

    /// 注册输入监听器（在 `TuiBase` 的输入管线里、聚焦组件之前按插入顺序运行；返回的
    /// consume 会终止本次分发）。
    fn add_input_listener(&mut self, listener: TuiInputListener) -> crate::tui::InputListenerId {
        self.base.add_input_listener(listener)
    }

    /// 按 id 注销输入监听器；id 不存在时无操作。
    fn remove_input_listener(&mut self, id: crate::tui::InputListenerId) {
        self.base.remove_input_listener(id);
    }

    /// 委托 `TuiBase` 查询终端前景/背景/调色板；超时以已收到的部分结果 resolve。
    fn query_terminal_colors(
        &mut self,
        timeout_ms: u64,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = crate::terminal_colors::TerminalColors> + '_>,
    > {
        self.base.query_terminal_colors(timeout_ms)
    }
}

impl ViewportTui for TuiAltScreen {
    /// 等价于 [`TuiAltScreen::set_layout_root_component`]（语义见该方法）。
    fn set_layout_root(&mut self, component: Option<Box<dyn Component>>) {
        self.set_layout_root_component(component);
    }
}

// ---------------------------------------------------------------------------
// 自由函数
// ---------------------------------------------------------------------------

/// 在共享组件上执行 `AltScreenSearchComponent` 的方法（动态类型向下转型）。
///
/// 组件已被替换成别的类型时返回 `None`，调用方按"没有搜索组件"处理；借用期间不能再
/// 借用同一组件（`borrow_mut` 会 panic）。
fn with_search_component<R>(
    component: &SharedComponent,
    f: impl FnOnce(&mut AltScreenSearchComponent) -> R,
) -> Option<R> {
    let mut borrowed = component.borrow_mut();
    let any = borrowed.as_any_mut()?;
    any.downcast_mut::<AltScreenSearchComponent>().map(f)
}

/// 剥掉行首的一个或多个 OSC 133 区域标记（`OSC133_ZONE_PREFIX`）。
///
/// 标记形如 `\x1b]133;A` / `B` / `C`，以 BEL（`\x07`）或 ST（`\x1b\\`）结尾；只剥离行首
/// 连续出现的这几种标记，遇到其它标签或没有合法终止符就停止，剩余内容原样返回。
fn strip_osc133_zone_prefix(line: &str) -> String {
    let mut rest = line;
    while let Some(after_prefix) = rest.strip_prefix("\x1b]133;") {
        let mut chars = after_prefix.char_indices();
        let Some((_, tag)) = chars.next() else {
            break;
        };
        if !matches!(tag, 'A' | 'B' | 'C') {
            break;
        }
        let after_tag = &after_prefix[tag.len_utf8()..];
        if let Some(next) = after_tag.strip_prefix('\x07') {
            rest = next;
            continue;
        }
        if let Some(next) = after_tag.strip_prefix("\x1b\\") {
            rest = next;
            continue;
        }
        break;
    }
    rest.to_string()
}

/// `OSC133_PROMPT_START = /^\x1b\]133;A(?:\x07|\x1b\\)/`。
fn is_osc133_prompt_start(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("\x1b]133;A") else {
        return false;
    };
    rest.starts_with('\x07') || rest.starts_with("\x1b\\")
}

/// `Math.round`（只处理正数；`f64::round` 与之相同）。
fn js_round(value: f64) -> f64 {
    value.round()
}

/// 深度优先收集布局盒树里的滚动视图（按出现顺序去重）。
///
/// 去重键是 `ScrollLayoutState` 的 `Rc` 指针：同一个 `ScrollView` 可能通过共享句柄在布局树
/// 里出现多次（每个布局条目各有一个 `LayoutNode::Scroll`），它们指向同一滚动状态，因此只
/// 收集/处理一次。
fn collect_layout_scroll_views(
    box_: &LayoutBox,
    seen: &mut HashSet<usize>,
    out: &mut Vec<Rc<dyn ScrollLayoutState>>,
) {
    if let Some(scroll_view) = box_.scroll_view.as_ref() {
        let key = Rc::as_ptr(scroll_view) as *const () as usize;
        if seen.insert(key) {
            out.push(Rc::clone(scroll_view));
        }
    }
    for child in &box_.children {
        collect_layout_scroll_views(child, seen, out);
    }
}

/// `Math.max(0, Math.min(max, value))`（TS 的选择点/点击列坐标夹取）。
///
/// `max < 0`（宽度/行数为 0）时 TS 得到 0；直接对 isize 调 `clamp` 会因 `min > max` panic，
/// 因此用 `min` 再 `max` 的两步写法。返回值总是落在 `[0, max.max(0)]`。
fn js_clamp_index(value: isize, max: isize) -> usize {
    value.min(max).max(0) as usize
}

/// TS 的 `try { callback() } catch {}`：宿主的 best-effort 回调（打开链接、右键粘贴）抛错
/// 只影响该次操作，界面继续；Rust 侧用 `catch_unwind` 吞掉 panic。
fn run_best_effort<F: FnOnce()>(callback: F) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(callback));
}

/// `/^\x1b\[<(\d+);(\d+);(\d+)([Mm])$/`。
///
/// 三段数字分别是按键码、1 基列、1 基行；返回时坐标转成 0 基。末尾 `M` 表示按下/移动、
/// `m` 表示松开。空串或含非 ASCII 数字段返回 `None`；数值用饱和运算解析（超大值不会溢出）。
fn parse_sgr_mouse_event(data: &str) -> Option<SgrMouseEvent> {
    let rest = data.strip_prefix("\x1b[<")?;
    let mut parts = rest.split(';');
    let button = parts.next()?;
    let x = parts.next()?;
    let y_and_final = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let (last, final_char) = {
        let mut chars = y_and_final.chars();
        let last_char = y_and_final.chars().last()?;
        let suffix_len = last_char.len_utf8();
        let digits = &y_and_final[..y_and_final.len() - suffix_len];
        let _ = &mut chars;
        (digits, last_char)
    };
    if !matches!(final_char, 'M' | 'm') {
        return None;
    }
    let parse = |text: &str| -> Option<i64> {
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let mut value: i64 = 0;
        for byte in text.bytes() {
            value = value
                .saturating_mul(10)
                .saturating_add(i64::from(byte - b'0'));
        }
        Some(value)
    };
    Some(SgrMouseEvent {
        button: parse(button)?,
        x: (parse(x)? - 1) as isize,
        y: (parse(last)? - 1) as isize,
        release: final_char == 'm',
    })
}

/// `parseWheelEvent(data)`。
///
/// 两种编码都支持：SGR（`\x1b[<…M/m`，按键码 bit 6 = 64 表示滚轮）和 X10 兼容的 6 字节
/// 序列（`\x1b[M` + 3 个字节；按键字节减 32 得到按键码，坐标字节减 33 得到 0 基坐标）。
/// 按键码低两位 0 = 上、1 = 下；其它方向（横向滚轮）返回 `None`。`direction` 输出 -1（上）/
/// 1（下）。
fn parse_wheel_event(data: &str) -> Option<WheelEvent> {
    if let Some(sgr) = parse_sgr_mouse_event(data) {
        let button = sgr.button;
        if button & 64 == 0 {
            return None;
        }
        let direction = button & 3;
        if direction != 0 && direction != 1 {
            return None;
        }
        return Some(WheelEvent {
            direction: if direction == 0 { -1 } else { 1 },
            x: sgr.x,
            y: sgr.y,
            button,
        });
    }
    let bytes = data.as_bytes();
    if bytes.len() == 6 && data.starts_with("\x1b[M") {
        let button = i64::from(bytes[3]) - 32;
        if button & 64 == 0 {
            return None;
        }
        let direction = button & 3;
        if direction != 0 && direction != 1 {
            return None;
        }
        return Some(WheelEvent {
            direction: if direction == 0 { -1 } else { 1 },
            x: isize::from(bytes[4]) - 33,
            y: isize::from(bytes[5]) - 33,
            button,
        });
    }
    None
}

/// `isMouseSequence(data)`：合法的 SGR 鼠标序列，或 6 字节的 `\x1b[M` 兼容序列。
///
/// 只做长度/前缀判断的分支（`\x1b[M`）即使坐标字节不规范也算鼠标序列；调用方对这类未
/// 识别的鼠标序列同样返回消费，不再交给组件输入。
fn is_mouse_sequence(data: &str) -> bool {
    if parse_sgr_mouse_event(data).is_some() {
        return true;
    }
    data.len() == 6 && data.starts_with("\x1b[M")
}
