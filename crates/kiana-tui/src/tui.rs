//! `tui.ts`：组件协议、鼠标事件、overlay、`TUI` 门面。
//!
//! 这是 `kiana-tui` 对外最重要的类型契约：`kiana-tools`（渲染器）、`kiana-interactive`（组件）、
//! `kiana-ext-api`（扩展 UI 上下文）都只依赖本模块的类型。
//!
//! ## 与 TS 的对应关系
//!
//! | TS | Rust |
//! |---|---|
//! | `interface Component` | [`Component`] trait（对象安全，`&mut self`） |
//! | `interface Focusable` | [`Focusable`] trait |
//! | `class Container` | [`Container`] |
//! | `interface TUI extends Component` | [`Tui`] trait |
//! | `interface OverlayHandle` | [`OverlayHandle`] trait（`show_overlay` 返回 `Box<dyn OverlayHandle>`） |
//! | `OverlayOptions` / `OverlayAnchor` / `OverlayMargin` / `SizeValue` | 同名类型 |
//! | `dispatchMouseEvent` / `retargetMouseEvent` | [`dispatch_mouse_event`] / [`retarget_mouse_event`] |
//! | `CURSOR_MARKER` / `FAKE_CURSOR_*` / `renderFakeCursor` | 同名常量与函数 |
//!
//! ## 与 TS 的三处有意偏差（实现时必须遵守）
//!
//! 1. **`dispatchMouseEvent` 不返回组件引用**。TS 的返回值带 `target.component`，Rust 里那会是
//!    `&mut dyn Component`，与容器的 `&mut self` 借用冲突。这里把命中的组件记成**地址**
//!    （[`MouseFocusTarget::Address`]），最外层分发者再用 [`TuiBase::focus_target_of_address`]
//!    解析成 [`FocusTarget`]（索引/ID）；几何信息在 [`TuiMouseDispatchResult`] 里。
//! 2. **`OverlayHandle` 是 trait**。TS 是匿名对象；Rust 由具体 TUI 实现提供结构体。
//! 3. **`Component` 不要求 `Send`**。整个 TUI 在单线程上运行（与 TS 的事件循环一致）；
//!    需要跨线程投递输入时用 `tokio::sync::mpsc` 把数据送回 UI 线程。
//!
//! 所有 `number` 字段在 TS 里都是 f64；坐标与尺寸在 Rust 里用 `usize`/`isize`，
//! 需要保留小数的（overlay 偏移、百分比）用 `f64`。TS 的 `Math.floor` 位置必须显式 `floor`。
//!
//! ## 本文件的内部结构（阅读顺序）
//!
//! 1. 组件协议：[`Component`]（渲染/输入/鼠标/容器结构判定）与 [`Focusable`]；
//!    [`Container`] 是最基础的堆叠容器，也是"按子项高度路由鼠标"的参考实现。
//! 2. 鼠标协议：事件结构、[`dispatch_mouse_event`] 与"组件地址"焦点归属
//!    （[`MouseFocusTarget`]），以及备用屏重放用的 `MouseDispatchTrace` 线程局部链。
//! 3. overlay/焦点核心：[`TuiCore`] 保存 overlay 栈与焦点恢复状态机；[`TuiOverlayHandle`]
//!    通过 `Rc<RefCell<TuiCore>>` 操作它，因此句柄可以独立于 [`TuiBase`] 存活。
//! 4. 宿主门面：[`TuiBase`] 持有真实组件树、终端与调度器，负责 `start`/`stop`、输入管线、
//!    渲染节流与终端颜色查询；具体渲染器（`main_screen.rs` / `alt_screen.rs`）持有它并
//!    实现 [`Tui`]。
//! 5. 渲染辅助：[`TuiBase::composite_overlays`] 与 [`composite_tui_line`]、
//!    [`TuiBase::apply_line_resets`]、[`TuiBase::resolve_fake_cursors`]、
//!    [`TuiBase::extract_cursor_position`]，以字符串与几何处理为主。其中
//!    [`TuiBase::composite_overlays`] 入口还会调用 [`TuiBase::flush_pending_focus_sync`]
//!    刷新挂起的焦点标志，并更新 overlay 的 `bounds` 与已渲染布局
//!    （`rendered_overlay_layouts`）；这些函数都不直接写终端。
//!
//! ## 一次输入的路径
//!
//! 1. `Terminal::start` 的输入回调把数据推进 `TuiBase` 的 `pending_input` 队列；宿主循环调用
//!    [`TuiBase::pump_pending_input`] 取出并逐条交给 [`TuiBase::handle_terminal_input`]。
//! 2. `handle_terminal_input` 的顺序固定为：刷新挂起焦点标志 → 终端颜色/明暗回复 →
//!    输入监听器 → cell 尺寸回复 → 调试键 → 修复不可见 overlay 的焦点 → 恢复被阻塞的
//!    overlay 焦点 → 交给聚焦组件。
//! 3. 组件只有在 [`Component::handles_input`] 为真时才收到输入；Kitty 按键释放事件还要求
//!    [`Component::wants_key_release`] 为真。
//!
//! ## 一次渲染的路径
//!
//! 1. [`TuiBase::request_render`] 在 `force = false` 的普通路径只置重绘标志；`force = true`
//!    时先 `reset_render_state()` 再 `request_immediate_render()`，下一次
//!    [`TuiBase::render_decision`] 直接返回 `Now`，绕过节流；普通路径则由它按
//!    [`MIN_RENDER_INTERVAL_MS`] 窗口决定等待或立即渲染。
//! 2. 具体渲染器渲染普通行后依次调用 [`TuiBase::resolve_fake_cursors`]、
//!    [`TuiBase::composite_overlays`]、[`TuiBase::extract_cursor_position`]、
//!    [`TuiBase::apply_line_resets`]；光标标记必须在行尾 reset 之前提取（见
//!    `main_screen.rs` 的模块文档）。
//! 3. 本模块不写终端：输出全部由 `TuiBase::terminal` 的 `Terminal` 实现完成。
//!
//! ## 共享状态与约束
//!
//! - 宿主状态用 `Rc`/`RefCell` 共享（[`TuiBase`]、[`TuiCore`]、overlay 句柄），这些类型
//!   不是 `Send`/`Sync`，必须在创建它们的线程上使用；纯数据的事件/选项结构不受此限。
//! - [`TuiCore`] 是 overlay 与焦点的唯一真相，但真实组件树在 [`TuiBase`] 里；overlay 句柄
//!   改完焦点只更新 `TuiCore` 并置 `focus_dirty`（overlay 组件自身的标志可以立即写），
//!   挂载树里真实组件的标志由 [`TuiBase::flush_pending_focus_sync`] 在下一个入口刷新
//!   （用于对齐 TS 的同步语义）。
//! - [`FocusTarget`] 是位置而非对象引用；[`MountState`] 记录每个目标最近一次对应的组件
//!   地址，用来复现 TS 的引用相等判定；被移出挂载树但仍有焦点的子树保存在 `DetachedFocus`。

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use crate::terminal_colors::{RgbColor, Terminal, TerminalColorScheme, TerminalColors};
use crate::terminal_image::{CellDimensions, get_capabilities, set_cell_dimensions};

/// 归一化后的鼠标事件类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TuiMouseEventType {
    Press,
    Release,
    Move,
    Drag,
    Click,
    Wheel,
}

/// 鼠标按键。`None` 用于移动与滚轮。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TuiMouseButton {
    Left,
    Middle,
    Right,
    None,
}

/// 归一化到单元格的鼠标事件；坐标从 0 开始。
///
/// TS 里 `x`/`y` 是相对接收组件的坐标，`screenX`/`screenY` 是终端绝对坐标，
/// `width`/`height` 是接收组件的当前边界。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuiMouseEvent {
    #[serde(rename = "type")]
    pub event_type: TuiMouseEventType,
    pub button: TuiMouseButton,
    /// 相对接收组件的坐标。
    pub x: isize,
    pub y: isize,
    /// 终端绝对坐标。
    pub screen_x: isize,
    pub screen_y: isize,
    /// 接收组件的当前宽度（单元格）。
    pub width: usize,
    /// 接收组件的当前高度（单元格）。
    pub height: usize,
    /// Shift 修饰键状态。
    pub shift: bool,
    /// Alt 修饰键状态。
    pub alt: bool,
    /// Ctrl 修饰键状态。
    pub ctrl: bool,
    /// 逻辑行数；负值向上滚。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wheel_delta: Option<f64>,
    /// `type == "click"` 时的连击次数。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub click_count: Option<u32>,
}

impl TuiMouseEvent {
    /// 复制事件并把坐标换算到 `target` 的局部坐标（`retargetMouseEvent`）。
    pub fn retarget(&self, target: &TuiMouseDispatchTarget) -> TuiMouseEvent {
        let mut next = self.clone();
        next.x = self.screen_x - target.origin_x;
        next.y = self.screen_y - target.origin_y;
        next.width = target.width;
        next.height = target.height;
        next
    }
}

/// 组件对鼠标事件的响应。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TuiMouseEventResult {
    /// 停止冒泡并抑制渲染器的兜底行为。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handled: Option<bool>,
    /// 后续 drag/release 路由到本组件；隐含 handled。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture: Option<bool>,
    /// 请求键盘焦点；隐含 handled。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus: Option<bool>,
    /// 显式请求/抑制一次渲染。move 与 release 默认 false，其余默认 true。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render: Option<bool>,
    /// `focus == true` 时，键盘焦点应交给谁（TS `TuiMouseDispatchResult.focusTarget`）。
    ///
    /// 转发型容器（把事件交给嵌套子控件、自己再转发按键的那种）在 `focus` 冒泡上来时把它
    /// 改成自己的 [`MouseFocusTarget::Address`]，避免把焦点交给一个随时可能被容器移除的
    /// 嵌套控件。`None` 与 `SelfComponent` 只出现在未经 [`dispatch_mouse_event`] 的结果里，
    /// 由调用方按"交给收到事件的那个组件"解释。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_target: Option<MouseFocusTarget>,
}

/// 鼠标焦点归属（TS `focusTarget: Component`）。
///
/// TS 的 `focusTarget` 是组件引用；Rust 的返回值不能携带 `&mut dyn Component`，因此在
/// [`dispatch_mouse_event`] 里把接收者记成它的**组件地址**（[`MouseFocusTarget::Address`]），
/// 由最外层分发者（它知道整棵树）解析成 [`FocusTarget`]。
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub enum MouseFocusTarget {
    /// 焦点交给返回该结果的组件本身（TS `focusTarget: this`，仅用于未经分发的裸结果）。
    #[default]
    SelfComponent,
    /// 组件地址（`dispatch_mouse_event` 的接收者）：TS `result.target.component` /
    /// 转发结果里的 `focusTarget`。最外层分发者用 [`TuiBase::focus_target_of_address`] 解析。
    Address(usize),
    /// 显式目标（调用方已经知道索引/overlay id）。
    Explicit(FocusTarget),
}

impl MouseFocusTarget {
    /// 结合"本次分发的接收者"解析成具体焦点目标。
    ///
    /// [`MouseFocusTarget::Address`] 需要整棵树才能解析，这里退回 `dispatched`；
    /// 真正的解析在 [`TuiBase::apply_mouse_focus_result`] 里完成。
    pub fn resolve(self, dispatched: FocusTarget) -> FocusTarget {
        match self {
            MouseFocusTarget::SelfComponent | MouseFocusTarget::Address(_) => dispatched,
            MouseFocusTarget::Explicit(target) => target,
        }
    }
}

impl TuiMouseEventResult {
    /// `handled || capture || focus`——TS `dispatchMouseEvent` 的「吞掉事件」判定。
    pub fn stops_propagation(&self) -> bool {
        self.handled == Some(true) || self.capture == Some(true) || self.focus == Some(true)
    }
}

/// 命中的组件几何信息。TS 还带 `component` 引用；Rust 侧由调用方用 [`FocusTarget`] 表达。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuiMouseDispatchTarget {
    /// 组件左上角的终端绝对列。
    pub origin_x: isize,
    /// 组件左上角的终端绝对行。
    pub origin_y: isize,
    /// 组件宽度（单元格）。
    pub width: usize,
    /// 组件高度（单元格）。
    pub height: usize,
}

/// 分发结果。`handled` 恒为 true。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuiMouseDispatchResult {
    /// 命中且事件已被吞掉；本模块构造的结果固定为 true。
    pub handled: bool,
    /// 命中的 overlay 根组件矩形（终端绝对坐标）。TS 在子组件转发时 `target` 指向**最内层**
    /// 组件；Rust 的冻结结果不保留该信息，[`TuiBase::dispatch_mouse_to_overlay`] 只填
    /// overlay 根组件的 `layout` 几何。
    pub target: TuiMouseDispatchTarget,
    /// 结果里 `focus == true` 时，焦点应交给承载事件的容器（TS 的 `focusTarget`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_target: Option<FocusTarget>,
}

/// 焦点对象：顶层容器子项、嵌套子项或 overlay。
///
/// TS 的 `focusTarget` 是组件引用（可以是任意深度的嵌套组件，`setFocus(editor)` 在
/// `tui.addChild(container)` 之后依然有效）。Rust 用索引表达：
/// - [`FocusTarget::Component`]：`TuiBase::children` 的下标（最常用，语义与骨架一致）；
/// - [`FocusTarget::Path`]：从顶层子项开始的下标路径（`Component { index }` 等价于长度为 1 的
///   路径），用于聚焦 `Container` 里的嵌套控件（`tui.ts` 的 `containsComponent` 递归语义）；
/// - [`FocusTarget::LayoutRoot`]：布局根（`TuiAltScreen.setLayoutRoot`，`tui-alt-screen.ts:328`）
///   内部的目标；空路径表示布局根本身。TS 里 `setLayoutRoot(new VStack([...editor...]))`
///   之后 `setFocus(editor)` 仍然有效（`test/tui-alt-screen.test.ts:780-810`）。
/// - [`FocusTarget::Overlay`]：overlay 栈项的稳定 id。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum FocusTarget {
    Component { index: usize },
    Path { path: FocusPath },
    LayoutRoot { path: FocusPath },
    Overlay { id: usize },
}

/// 焦点路径的最大深度（超过时聚焦仍可用，但裁剪到该深度）。
pub const FOCUS_PATH_MAX_DEPTH: usize = 16;

/// 从顶层子项开始的固定容量下标路径（保持 `Copy`，以便 [`FocusTarget`] 维持 `Copy`）。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub struct FocusPath {
    indices: [u32; FOCUS_PATH_MAX_DEPTH],
    depth: u8,
}

impl FocusPath {
    /// 空路径：在 [`FocusTarget::LayoutRoot`] 里表示布局根本身。
    pub fn root() -> Self {
        FocusPath::default()
    }

    /// 由下标切片构造；空切片返回 `None`（空路径请用 [`FocusPath::root`]）。超长路径截断到
    /// [`FOCUS_PATH_MAX_DEPTH`]（保留前缀），单个下标超出 `u32` 时饱和为 `u32::MAX`。
    pub fn new(indices: &[usize]) -> Option<Self> {
        if indices.is_empty() {
            return None;
        }
        let mut path = FocusPath::default();
        let depth = indices.len().min(FOCUS_PATH_MAX_DEPTH);
        for (slot, index) in path.indices.iter_mut().zip(&indices[..depth]) {
            *slot = u32::try_from(*index).unwrap_or(u32::MAX);
        }
        path.depth = depth as u8;
        Some(path)
    }

    /// 路径下标。
    pub fn indices(&self) -> &[u32] {
        &self.indices[..usize::from(self.depth)]
    }

    /// 去掉最后一节（根路径返回 `None`）。
    pub fn parent(&self) -> Option<FocusPath> {
        let depth = usize::from(self.depth);
        if depth <= 1 {
            return None;
        }
        let mut path = *self;
        path.depth = (depth - 1) as u8;
        Some(path)
    }

    /// 在末尾追加一节（超深时返回 `None`）。
    pub fn child(&self, index: usize) -> Option<FocusPath> {
        let depth = usize::from(self.depth);
        if depth >= FOCUS_PATH_MAX_DEPTH {
            return None;
        }
        let mut path = *self;
        path.indices[depth] = u32::try_from(index).ok()?;
        path.depth = (depth + 1) as u8;
        Some(path)
    }
}

impl From<usize> for FocusPath {
    fn from(index: usize) -> Self {
        FocusPath::new(&[index]).expect("单元素路径总是有效")
    }
}

/// 键盘输入监听器的返回值：`consume` 阻止后续处理，`data` 替换这段输入。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TuiInputListenerResult {
    /// `Some(true)` 时终止输入处理（后续监听器与聚焦组件都收不到这段输入）。
    pub consume: Option<bool>,
    /// 用返回的字符串替换当前输入；多个监听器按注册顺序链式替换。
    pub data: Option<String>,
}

/// 键盘输入监听器。
pub type TuiInputListener = Box<dyn FnMut(&str) -> Option<TuiInputListenerResult>>;

/// 监听器句柄（`addInputListener` 的返回值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InputListenerId(pub usize);

/// 终端渲染组件（TS `Component`）。
///
/// 默认实现逐条对应 TS 的可选成员：`handle_input` 空、`handle_mouse` 返回 `None`、
/// `wants_key_release` 为 false、`invalidate` 空。
///
/// 对象安全：所有方法都接受 `&mut self`/`&self`，不出现 `Self: Sized` 约束。
pub trait Component {
    /// 按视口宽度渲染成若干行（每行可含 ANSI）。
    fn render(&mut self, width: usize) -> Vec<String>;

    /// 获得焦点时的键盘输入处理。
    fn handle_input(&mut self, _data: &str) {}

    /// 归一化鼠标事件处理。
    fn handle_mouse(&mut self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }

    /// 是否接收 Kitty 协议的按键释放事件（默认 false）。
    fn wants_key_release(&self) -> bool {
        false
    }

    /// 是否实现了有意义的键盘输入处理（TS `"handleInput" in component`）。
    ///
    /// TS 里 `handleInput` 是可选成员：静态展示组件没有它，`handleTerminalInput` 既不会调用
    /// 它、也不会为它请求立即渲染（`tui.ts:1117-1156`）。Rust 的 trait 默认方法让"有没有实现"
    /// 无法反射判定，因此这里显式声明：默认 `true`（保守，等价于有处理器），纯展示组件应
    /// 覆写为 `false` 以复现 TS 的跳过行为。
    ///
    /// [`Container`] 覆写为 `false`：TS 的 `Container`/`Stack`/`ScrollView` 都没有
    /// `handleInput`（只有 `TuiBase` 子类有）。**包装 `Container` 的转发组件**若在 TS 里定义了
    /// `handleInput`，转发后要自己把结果的 `focus_target` 改成自身的
    /// [`MouseFocusTarget::Address`]（TS `Container.handleMouse` 的
    /// `if (result?.focus && this.handleInput) return { ...result, focusTarget: this }`）。
    fn handles_input(&self) -> bool {
        true
    }

    /// 丢弃缓存渲染状态（主题切换或需要全量重绘时调用）。
    fn invalidate(&mut self) {}

    /// TS 的可选 `dispose?()`：宿主替换 / 清空组件时调用。
    ///
    /// TS 的 `Component` 接口没有 `dispose`，宿主按鸭子类型（`component.dispose?.()`）调用；
    /// Rust 用带默认空实现的 trait 方法表达同一可选钩子（释放定时器、终端监听器等）。
    fn dispose(&mut self) {}

    /// TS 的鸭子判定属性 `component.width`（`showExtensionCustom` 在未提供
    /// `overlayOptions` 时用它作为 overlay 宽度）。没有该属性的组件返回 `None`。
    fn width(&self) -> Option<f64> {
        None
    }

    /// 需要 `instanceof` 等价的向下转型时由实现返回 `Some(self)`。
    ///
    /// **焦点/挂载遍历不要用它**：TS 的 `containsComponent` 走 `instanceof Container`，而
    /// Rust 侧 `Stack`/`ScrollView` 等容器型组件的具体类型各不相同，遍历请用
    /// [`Component::container_children`]。
    fn as_any(&self) -> Option<&dyn Any> {
        None
    }

    /// 可变向下转型。
    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        None
    }

    /// TS `instanceof Container` 的结构判定：返回子组件列表（渲染顺序）。
    ///
    /// TS 里 `Container`、`Stack`（VStack/HStack）、`ScrollView`、`TuiBase` 都继承
    /// `Container`，所以 `containsComponent`/`setFocus` 能看见它们的子组件
    /// （`tui.ts:738-746`）。Rust 的默认实现返回 `None`：只有一个子项容器的组件
    /// （[`Container`]，以及 components 单元里包装 `Container` 的 Stack/ScrollView）必须
    /// 覆写本方法与 [`Component::container_children_mut`]，否则焦点路径无法穿过它们。
    ///
    /// **树遍历请用 [`Component::for_each_container_child`]**：共享句柄代理（Stack/ScrollView
    /// 把子项包成 `Rc<RefCell<dyn Component>>`）无法通过本方法借出底层子项，只能由代理把
    /// 遍历透传下去；直接读本方法会在第二层嵌套容器处停住。
    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        None
    }

    /// [`Component::container_children`] 的可变版本（写 `focused` 标志时需要）。
    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        None
    }

    /// `handleMouse` 转发目标的可变子项列表（**仅**用于鼠标重放导航）。
    ///
    /// 与 [`Component::container_children_mut`] 的区别：TS 里 `Box` 不是 `Container`
    /// （`containsComponent`/`setFocus` 看不见它的子项），但 `Box.handleMouse` 会把事件转发
    /// 给子项。备用屏重放 capture/按下目标时沿该列表下钻，语义等价于 `dispatchMouseEvent`
    /// 返回的 `target.component`。默认与容器子项相同。
    fn mouse_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        self.container_children_mut()
    }

    /// 只读遍历 TS `Container` 子组件；回调返回 `false` 时提前终止。
    ///
    /// 默认实现遍历 [`Component::container_children`]。焦点/挂载遍历必须用本方法而不是
    /// 直接读 [`Component::container_children`]：`Stack`/`ScrollView` 的子项是共享句柄
    /// 代理（见 `components::stack` 的 [`crate::components::stack`] 模块文档），代理自身
    /// 不是容器，但它包装的底层组件在 TS 里就是真实子对象。代理覆写本方法把遍历**透传**
    /// 给底层组件，因此路径层级与真实父子结构一致，任意深度的嵌套容器都
    /// 能被 `containsComponent` / `setFocus` / 鼠标焦点解析穿过。
    fn for_each_container_child(&self, f: &mut dyn FnMut(&dyn Component) -> bool) {
        if let Some(children) = self.container_children() {
            for child in children {
                if !f(child.as_ref()) {
                    break;
                }
            }
        }
    }

    /// [`Component::for_each_container_child`] 的可变版本。
    fn for_each_container_child_mut(
        &mut self,
        f: &mut dyn FnMut(&mut (dyn Component + 'static)) -> bool,
    ) {
        if let Some(children) = self.container_children_mut() {
            for child in children.iter_mut() {
                if !f(child.as_mut()) {
                    break;
                }
            }
        }
    }

    /// `isFocusable` 的 Rust 等价物：可获得焦点的组件返回 `Some(self)`。
    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        None
    }

    /// TS `[LAYOUT_NODE]()` 鸭子判定的等价物（`getLayoutNode`）：实现布局协议的组件返回
    /// `Some(LayoutNode)`，其余返回 `None`。
    ///
    /// 每次调用都应**重新构造**节点（TS 的节点是即时视图，不是缓存对象）。见
    /// `spec/tui-components.md` §5.1 与 `spec/tui-core.md` §2.1。
    fn layout_node(&self) -> Option<crate::layout_node::LayoutNode> {
        None
    }
}

/// 可获得焦点并绘制硬件光标的组件（TS `Focusable`）。
pub trait Focusable {
    /// 焦点变化时由 TUI 设置；为 true 时组件应在光标处输出 [`CURSOR_MARKER`]。
    fn focused(&self) -> bool;
    /// 由 TUI 调用。
    fn set_focused(&mut self, focused: bool);
}

/// TS 的 `isFocusable` 类型守卫的 Rust 等价物（需要 `&mut`，因为返回的是可变引用）。
pub fn is_focusable(component: &mut dyn Component) -> bool {
    component.as_focusable().is_some()
}

/// 光标位置标记（APC 序列，终端忽略，宽度为 0）。
pub const CURSOR_MARKER: &str = "\x1b_kiana:c\x07";
/// 伪光标起始标记。
pub const FAKE_CURSOR_START: &str = "\x1b_kiana:fc\x07";
/// 伪光标结束标记。
pub const FAKE_CURSOR_END: &str = "\x1b_kiana:/fc\x07";

/// 用伪光标标记包住文本（`renderFakeCursor`）。
pub fn render_fake_cursor(text: &str) -> String {
    format!("{FAKE_CURSOR_START}{text}{FAKE_CURSOR_END}")
}

// TS `tui.ts:209` 的独立再导出 `export { visibleWidth };`（模块级 API，`index.ts` 走
// `./utils.ts` 自己那份；这里镜像 tui 模块，不影响 index 的 176 个符号计数）。
pub use crate::text::visible_width;

/// overlay 锚点（TS `OverlayAnchor`）。
///
/// 只在没有显式 `row`/`col` 时参与定位：锚点在"终端尺寸减去 `margin`"得到的可用区域内
/// 摆放 overlay，之后叠加 `offset_x`/`offset_y`，最终位置仍会被夹回边距内。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayAnchor {
    /// 水平与垂直都居中（默认）。
    #[default]
    Center,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    TopCenter,
    BottomCenter,
    LeftCenter,
    RightCenter,
}

/// overlay 四边留白。
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OverlayMargin {
    /// 上边距（单元格）；`None` 视为 0，负值在布局时夹到 0。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top: Option<f64>,
    /// 右边距（单元格）；`None` 视为 0，负值在布局时夹到 0。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right: Option<f64>,
    /// 下边距（单元格）；`None` 视为 0，负值在布局时夹到 0。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bottom: Option<f64>,
    /// 左边距（单元格）；`None` 视为 0，负值在布局时夹到 0。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left: Option<f64>,
}

/// 每边的留白值；TS 的 `margin` 允许直接用数字（四边相同）。
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum OverlayMarginValue {
    /// 四边使用同一个数值（TS `margin: 2`）。
    All(f64),
    /// 逐边指定（TS `margin: { top, right, bottom, left }`）。
    Sides(OverlayMargin),
}

/// 绝对值或百分比（TS 模板字符串 `"50%"`）。
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum SizeValue {
    /// 绝对值（单元格或行）。
    Absolute(f64),
    /// 参考尺寸的百分比：`50.0` 表示 50%，[`SizeValue::resolve`] 时向下取整。
    Percent(f64),
}

impl SizeValue {
    /// `parseSizeValue`：按参考尺寸求绝对值，百分比向下取整（`Math.floor`）。
    pub fn resolve(self, reference: f64) -> f64 {
        match self {
            SizeValue::Absolute(v) => v,
            SizeValue::Percent(p) => (reference * p / 100.0).floor(),
        }
    }
}

/// overlay 的定位与尺寸选项（TS `OverlayOptions`）。
///
/// `visible` 是回调，因此本结构**不实现** `Clone`/`PartialEq`；需要复制时由实现显式逐字段复制。
#[derive(Default)]
pub struct OverlayOptions {
    /// overlay 宽度；百分比按终端宽度解析。`None` 时取 `min(80, 可用宽度)`。
    pub width: Option<SizeValue>,
    /// 宽度下限；在 `width` 解析之后、最终夹取到 `[1, 可用宽度]` 之前应用。
    pub min_width: Option<f64>,
    /// 最大高度；百分比按终端高度解析并夹到 `[1, 可用高度]`，渲染出的行数再按
    /// `Array.prototype.slice` 的语义截断（向零取整）。
    pub max_height: Option<SizeValue>,
    /// 未指定 `row`/`col` 时的锚点（默认 [`OverlayAnchor::Center`]）。
    pub anchor: Option<OverlayAnchor>,
    /// 在 `row`/`col` 定位之后叠加的列偏移（可为负），最终仍被夹回左右边距内。
    pub offset_x: Option<f64>,
    /// 在 `row`/`col` 定位之后叠加的行偏移（可为负），最终仍被夹回上下边距内。
    pub offset_y: Option<f64>,
    /// 纵向定位；百分比相对可用高度（0% 靠上，100% 靠下），按 TS 的
    /// `maxRow * (percent / 100)` 顺序计算后向下取整。
    pub row: Option<SizeValue>,
    /// 横向定位；百分比相对可用宽度（0% 靠左，100% 靠右），计算方式同 [`OverlayOptions::row`]。
    pub col: Option<SizeValue>,
    /// 四边留白；负值在布局时夹到 0。
    pub margin: Option<OverlayMarginValue>,
    /// 仅在返回 true 时渲染；每次渲染都会以当前终端尺寸调用。
    pub visible: Option<Box<dyn Fn(usize, usize) -> bool>>,
    /// 不抢占键盘焦点。
    pub non_capturing: Option<bool>,
}

/// `unfocus` 的选项。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OverlayUnfocusOptions {
    /// 显式指定释放后聚焦的对象；`Some` 时以 `target` 为准，`target: None` 请求清除
    /// 焦点（测试 `explicit_null_unfocus_target_clears_focus_without_restoring_overlays`）。
    /// 正常释放路径下，只有 `options` 整体为 `None` 才走"最上层可见的捕获型 overlay →
    /// 打开前焦点"的回退链。
    pub target: Option<FocusTarget>,
}

/// 最近一次渲染出的、相对终端的 overlay 矩形。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OverlayBounds {
    pub row: usize,
    pub col: usize,
    pub width: usize,
    pub height: usize,
}

/// `showOverlay` 返回的句柄（TS `OverlayHandle`）。
///
/// 由具体 TUI 实现提供结构体；本 trait 只固定调用面。
pub trait OverlayHandle {
    /// 永久移除（不能再次显示）。
    fn hide(&mut self);
    /// 临时隐藏/显示。
    ///
    /// 隐藏聚焦中的 overlay 会把焦点交给最上层可见的捕获型 overlay
    /// （[`TuiCore::topmost_visible_overlay`] 跳过 `non_capturing`；没有则回退 `preFocus`）；
    /// 显示一个可见的捕获型 overlay 会把它重新聚焦并置于视觉最前。
    fn set_hidden(&mut self, hidden: bool);
    /// 是否被 [`OverlayHandle::set_hidden`] 临时隐藏（与 `visible` 回调无关）。
    fn is_hidden(&self) -> bool;
    /// 聚焦并置于视觉最前；overlay 不存在或不可见时为 no-op。
    fn focus(&mut self);
    /// 释放焦点。
    ///
    /// 仅当本 overlay 正持有焦点、或它还有挂起的恢复状态时才生效；两者都不成立时是
    /// no-op（测试 `unfocus_when_overlay_does_not_have_focus_is_a_no_op`）。
    ///
    /// 正常释放（本 overlay 正持有焦点，或调用方给了 `options`）时：`options` 为 `None`
    /// 表示交给最上层可见的捕获型 overlay（[`TuiCore::topmost_visible_overlay`] 跳过
    /// `non_capturing`，并排除自己），没有则回到打开前的焦点；`options` 为 `Some` 时以
    /// 显式 `target` 为准（`target: None` 表示清除焦点）。若焦点已被其他组件抢走（恢复
    /// 状态为 `Blocked`），本调用只改写或清除恢复策略，不立即改变焦点，等后续结算时生效。
    fn unfocus(&mut self, options: Option<OverlayUnfocusOptions>);
    /// 键盘焦点是否正指向这个 overlay（不检查它当前是否可见）。
    fn is_focused(&self) -> bool;
    /// 最近一次渲染的边界；overlay 不可见或尚未渲染过时为 `None`。
    fn bounds(&self) -> Option<OverlayBounds>;
}

/// 容器组件（TS `Container`）：按顺序垂直堆叠子组件，并把鼠标事件按子项高度路由。
///
/// TS 的 `mouseLayout` 缓存（上一轮渲染的宽度与子项高度）在 Rust 里保留为
/// [`Container::mouse_layout`]，语义相同：宽度未变时复用，避免重复 `render`。
///
/// 与 TS 的一处表示差异：TS 的缓存项持有**组件引用**，所以 `removeChild` 之后旧条目仍指向
/// 被移除的组件（`children.splice` 不动 `mouseLayout`，`tui.ts:360-370`）。Rust 无法在缓存里
/// 存 `&mut` 引用，这里用**稳定的子项 id**（[`Container::child_ids`]，只增不减、与组件同生命周期
/// 绑定到槽位）代替引用，使排在被删子项之后的条目仍解析到正确的组件、被删子项自己的行不再命中
/// 任何组件——后者与 TS 的"命中已移除组件"在可观察行为上等价（该组件已不在树中，事件无副作用）。
#[derive(Default)]
pub struct Container {
    pub children: Vec<Box<dyn Component>>,
    /// 与 `children` 平行的稳定 id（由 `Container::next_child_id` 递增分配）。
    pub child_ids: Vec<u64>,
    /// `(width, [(child_id, height)])`；仅用于鼠标路由。
    pub mouse_layout: Option<(usize, Vec<(u64, usize)>)>,
    next_child_id: u64,
}

impl Container {
    /// 空容器（没有子项，也没有鼠标布局缓存）。
    pub fn new() -> Self {
        Container::default()
    }

    /// 追加子项并分配单调递增的稳定 id（鼠标布局缓存用 id 而不是下标）。
    pub fn add_child(&mut self, component: Box<dyn Component>) {
        self.next_child_id += 1;
        self.child_ids.push(self.next_child_id);
        self.children.push(component);
    }

    /// 按引用移除：TS 的 `indexOf` 用引用相等，Rust 侧用指针相等。
    ///
    /// **不动 `mouse_layout`**（缓存里的是 id，不是下标，所以移除后不会把
    /// 命中错误地转给后续子项）。
    pub fn remove_child(&mut self, component: &dyn Component) {
        let target = component as *const dyn Component as *const ();
        if let Some(idx) = self
            .children
            .iter()
            .position(|c| c.as_ref() as *const dyn Component as *const () == target)
        {
            self.children.remove(idx);
            self.child_ids.remove(idx);
        }
    }

    /// 按槽位下标移除（把手柄/外部索引传进来时用）。
    ///
    /// 返回被移除的组件；同步移除对应的稳定 id，但**不动** `mouse_layout`（与
    /// [`Container::remove_child`] 一致）。
    pub fn remove_child_at(&mut self, index: usize) -> Option<Box<dyn Component>> {
        if index >= self.children.len() {
            return None;
        }
        self.child_ids.remove(index);
        Some(self.children.remove(index))
    }

    /// 清空子项、稳定 id 与鼠标布局缓存。
    pub fn clear(&mut self) {
        self.children.clear();
        self.child_ids.clear();
        self.mouse_layout = None;
    }

    /// 某个子项当前的稳定 id（`None` 表示它不在 `children` 里）。
    pub fn child_id_of(&self, component: &dyn Component) -> Option<u64> {
        let target = component as *const dyn Component as *const ();
        let index = self
            .children
            .iter()
            .position(|c| c.as_ref() as *const dyn Component as *const () == target)?;
        self.child_ids.get(index).copied()
    }
}

impl Component for Container {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let mut layout = Vec::with_capacity(self.children.len());
        // 每轮渲染同时重建鼠标布局缓存：(宽度, [(子项稳定 id, 该子项渲染出的行数)])。
        // 缓存用 id 而不是下标，子项被删除后旧条目不会把事件错投给后续子项。
        for (index, child) in self.children.iter_mut().enumerate() {
            let child_lines = child.render(width);
            let id = self.child_ids.get(index).copied().unwrap_or(index as u64);
            layout.push((id, child_lines.len()));
            lines.extend(child_lines);
        }
        self.mouse_layout = Some((width, layout));
        lines
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if event.y < 0 || event.y as usize >= event.height {
            return None;
        }
        // 宽度未变时复用上一轮布局；否则现场渲染求高度（与 TS 相同）。
        let heights: Vec<(u64, usize)> = match &self.mouse_layout {
            Some((w, layout)) if *w == event.width => layout.clone(),
            _ => self
                .children
                .iter_mut()
                .enumerate()
                .map(|(index, c)| {
                    let id = self.child_ids.get(index).copied().unwrap_or(index as u64);
                    (id, c.render(event.width).len())
                })
                .collect(),
        };
        let mut child_y: isize = 0;
        for (id, height) in heights {
            if event.y >= child_y && event.y < child_y + height as isize {
                let Some(index) = self.child_ids.iter().position(|candidate| *candidate == id)
                else {
                    // 缓存项指向已移除的子项：TS 会把事件交给那个已脱离树的组件（无副作用），
                    // Rust 里没有可交付的对象，视为未命中并继续向下找。
                    child_y += height as isize;
                    continue;
                };
                let mut child_event = event.clone();
                child_event.y = event.y - child_y;
                child_event.height = height;
                let result = dispatch_mouse_event(self.children[index].as_mut(), &child_event);
                // TS `Container.handleMouse`：转发型容器在子控件请求焦点时把焦点留给自己
                // （`if (result?.focus && this.handleInput) return { ...result, focusTarget: this }`）。
                // TS 的 `Container`/`Stack`/`ScrollView` **没有** `handleInput`，所以这个分支
                // 永远不成立，事件结果的 `focusTarget` 保持为**命中的子控件**
                // （`tui.ts:383-403`）；Rust 侧 `Container::handles_input` 返回 false 复现它。
                return result.map(|mut result| {
                    if result.focus == Some(true) && self.handles_input() {
                        let address = self as *const Container as *const () as usize;
                        result.focus_target = Some(MouseFocusTarget::Address(address));
                    }
                    result
                });
            }
            child_y += height as isize;
        }
        None
    }

    fn invalidate(&mut self) {
        for child in self.children.iter_mut() {
            child.invalidate();
        }
        self.mouse_layout = None;
    }

    /// TS 的 `class Container` **没有** `handleInput`（只有 `TuiBase` 子类才有），因此
    /// `handleTerminalInput` 不会把输入交给它、`Container.handleMouse` 也不会把焦点收回。
    fn handles_input(&self) -> bool {
        false
    }

    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        Some(&self.children)
    }

    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        Some(&mut self.children)
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

/// 段重置：SGR 重置 + 关闭 OSC 8 链接（TS `SEGMENT_RESET`）。
pub const SEGMENT_RESET: &str = "\x1b[0m\x1b]8;;\x07";

/// `compositeTuiLine(baseLine, overlayLine, startCol, overlayWidth, totalWidth)`：
/// 把 overlay 行合成到基线的固定列区间：overlay 内容从 `startCol` 开始**左对齐**，右侧
/// 补空格到 `overlayWidth`；基线中该区间之外的列原样保留（ANSI 状态由 `extract_segments`
/// 延续），总宽超出 `totalWidth` 时整行按列截断。
///
/// 基行是图片行时原样返回（图片行不能按列切分）。
pub fn composite_tui_line(
    base_line: &str,
    overlay_line: &str,
    start_col: isize,
    overlay_width: usize,
    total_width: usize,
) -> String {
    if crate::terminal_image::is_image_line(base_line) {
        return base_line.to_string();
    }

    // 阶段 1：把基线按 [0, start) / [start, start+overlay_width) / 之后 三段切开；
    // `strict_after = true` 要求 after 只收完整落在区间内的字素，避免半个宽字符。
    let start = usize::try_from(start_col).unwrap_or(0);
    let after_start = start + overlay_width;
    let after_len = total_width.saturating_sub(after_start);
    let base = crate::text::extract_segments(base_line, start, after_start, after_len, true);
    // 阶段 2：overlay 行按列宽裁剪；三段各自补齐到目标宽度（TS 的 max(0, …) 公式）。
    let overlay = crate::text::slice_with_width(overlay_line, 0, overlay_width, true);
    let before_pad = start.saturating_sub(base.before_width);
    let overlay_pad = overlay_width.saturating_sub(overlay.width);
    let actual_before_width = start.max(base.before_width);
    let actual_overlay_width = overlay_width.max(overlay.width);
    let after_target = total_width.saturating_sub(actual_before_width + actual_overlay_width);
    let after_pad = after_target.saturating_sub(base.after_width);
    let result = format!(
        "{}{}{SEGMENT_RESET}{}{}{SEGMENT_RESET}{}{}",
        base.before,
        " ".repeat(before_pad),
        overlay.text,
        " ".repeat(overlay_pad),
        base.after,
        " ".repeat(after_pad),
    );
    // 阶段 3：宽字素的边界误差可能让总宽超出 total_width，按 TS 的兜底再整行裁剪一次。
    if crate::text::visible_width(&result) <= total_width {
        result
    } else {
        crate::text::slice_by_column(&result, 0, total_width, true)
    }
}

/// 渲染模式：主屏（正常屏幕缓冲区）或备用屏（全屏）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TuiMode {
    /// 主屏模式（`TuiMainScreen`）。
    Regular,
    /// 全屏/备用屏模式（`TuiAltScreen`）。
    Fullscreen,
}

/// `stop()` 的选项。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TuiStopOptions {
    /// 把渲染输出留在终端上，供接管同一终端的下一个 TUI 使用。
    pub preserve_screen: bool,
}

/// `VIEWPORT_TUI` 符号的等价位（TS `Symbol.for("@earendil-works/kiana-tui/viewport")`）。
///
/// TS 把该 symbol 当作只读标记属性放在 `TuiAltScreen` 上；Rust 没有 symbol 属性，
/// 改用 [`Tui::is_viewport_tui`] 这个默认方法表达同一判定，并保留本常量作为
/// 协议标识字符串（跨语言日志/调试用）。
pub const VIEWPORT_TUI: &str = "@earendil-works/kiana-tui/viewport";

/// TUI 门面（TS `interface TUI extends Component`）。
///
/// 具体实现（`TuiMainScreen` / `TuiAltScreen`）在 `main_screen.rs` / `alt_screen.rs`。
/// 下游通过 `&mut dyn Tui` / `Rc<RefCell<dyn Tui>>` 使用它（见 `kiana-interactive` 的
/// `tui_renderer.rs`），因此所有方法都必须保持对象安全。
pub trait Tui: Component {
    /// 渲染模式（主屏 / 备用屏）。
    fn mode(&self) -> TuiMode;
    /// 顶层子组件列表。
    ///
    /// 裸改（`push`/`remove`/重排）可能绕过实现方的挂载快照、鼠标布局与焦点簿记；
    /// 结构化增删请优先用 [`Tui::add_child`] / [`Tui::remove_child`] /
    /// [`Tui::remove_child_at`]。
    fn children(&mut self) -> &mut Vec<Box<dyn Component>>;
    /// 顶层子项数量。
    ///
    /// 代理型 `Tui`（例如扩展宿主里的 `TuiArcAdapter`）无法穿过 `RefCell` 借出
    /// `&mut Vec`，但可以覆写本方法避免调用 [`Tui::children`] 的 panic 路径。
    fn children_len(&mut self) -> usize {
        self.children().len()
    }
    /// 在持有实现方内部借用的前提下访问顶层子项。
    ///
    /// 默认实现直接转发 [`Tui::children`]；代理实现覆写它，把回调交给真正的渲染器。
    /// 回调返回后借用必须结束，因此不要在回调里再次调用 `self` 的 `children` 类方法。
    fn with_children(&mut self, f: &mut dyn FnMut(&mut Vec<Box<dyn Component>>)) {
        f(self.children())
    }
    /// 追加顶层子组件；具体实现负责刷新挂载/焦点簿记。
    fn add_child(&mut self, child: Box<dyn Component>);
    /// 按组件身份移除；被删子树是否保活或继续持有焦点由实现决定（见各实现的文档）。
    fn remove_child(&mut self, child: &dyn Component);
    /// 按下标移除子项（[`Tui::remove_child`] 的下标等价物）。
    ///
    /// 扩展 UI 的 `tui.removeChild(component)` 只能按记录的身份找到下标，拿不到挂载组件的
    /// `&dyn Component`；具体渲染器覆写它，以便同步子项 id、鼠标布局与焦点状态。
    fn remove_child_at(&mut self, index: usize) -> Option<Box<dyn Component>> {
        let children = self.children();
        if index < children.len() {
            Some(children.remove(index))
        } else {
            None
        }
    }
    /// 尝试清空顶层子组件（TS `clear()`）；具体实现可能因焦点保留而提前停止，把尚未
    /// 移除的子项留在 `children` 里（如 `main_screen.rs` 的 `clear_children` 逐个调用
    /// `remove_child_at(0)`，遇到持焦子项返回 `None` 即结束）。焦点语义按实现处理
    /// （见各实现文档）。
    fn clear_children(&mut self);
    /// 当前键盘焦点目标（TS `getFocusedComponent`）。
    fn focus(&self) -> Option<FocusTarget>;
    /// 设置键盘焦点；`None` 请求清除焦点（TS `setFocus(component | null)`）。但若存在
    /// `Blocked` 且 `blocked_by` 正是当前焦点的恢复状态，[`TuiCore::set_focus_internal`]
    /// 可能先结算它，把焦点交回 overlay（测试
    /// `blocked_replacement_set_focus_null_resumes_the_visible_overlay`）。
    fn set_focus(&mut self, target: Option<FocusTarget>);
    /// 压入 overlay 并返回控制句柄；捕获型 overlay 在可见时立即获得焦点
    /// （TS `showOverlay`）。
    fn show_overlay(
        &mut self,
        component: Box<dyn Component>,
        options: OverlayOptions,
    ) -> Box<dyn OverlayHandle>;
    /// 移除最近压入的 overlay（TS `hideOverlay`）；若它正持有焦点，把焦点交给最上层
    /// 可见的捕获型 overlay（`topmost_visible_overlay` 跳过 `non_capturing`）或它的
    /// `preFocus`。
    fn hide_overlay(&mut self);
    /// 是否存在**可见** overlay（TS `hasOverlay`）；被隐藏的 overlay 不计入。
    fn has_overlay(&self) -> bool;
    /// 启动终端并按顺序执行生命周期钩子（TS `start()`）。
    fn start(&mut self);
    /// 停止终端；`options` 会传给生命周期钩子，是否保留屏幕由具体渲染器决定
    /// （TS `stop(options)`）。
    fn stop(&mut self, options: TuiStopOptions);
    /// 立即渲染一帧（TS `renderNow(force)`）；`force` 为真时先重置渲染器缓存。
    fn render_now(&mut self, force: bool);
    /// 请求渲染（TS `requestRender(force)`）；`force` 走"重置状态 + 立即"路径。
    fn request_render(&mut self, force: bool);
    /// 注册键盘输入监听器并返回注销用的 id；按注册顺序调用。
    fn add_input_listener(&mut self, listener: TuiInputListener) -> InputListenerId;
    /// 注销输入监听器（TS `removeInputListener`）；未知 id 为 no-op。
    fn remove_input_listener(&mut self, id: InputListenerId);
    /// 查询终端前景/背景/调色板（OSC 10/11/4 + DA1）。
    ///
    /// TS 返回 Promise；超时后 resolve 为已收到的部分（可能为空）。Rust 用 boxed future，
    /// 借 `&mut self` 直到完成。
    fn query_terminal_colors(
        &mut self,
        timeout_ms: u64,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = crate::terminal_colors::TerminalColors> + '_>,
    >;

    /// 是否为"视口 TUI"（TS `isViewportTUI(tui)`：`tui[VIEWPORT_TUI] === true`）。
    ///
    /// 只有备用屏渲染器（`TuiAltScreen`）返回 `true`；主屏渲染器保持默认 `false`。
    /// 取代 TS 的 symbol 标记属性（见 [`VIEWPORT_TUI`]）。
    fn is_viewport_tui(&self) -> bool {
        false
    }
}

/// `isViewportTUI(tui)`：判定某个 TUI 是否为视口 TUI（备用屏）。
pub fn is_viewport_tui(tui: &dyn Tui) -> bool {
    tui.is_viewport_tui()
}

/// 带布局根的 TUI（TS `interface ViewportTUI extends TUI`）。
///
/// 只有备用屏渲染器实现它：`render(width)` 变为 `layoutRoot?.render(width) ?? super.render(width)`，
/// 且 `getMountedRoots()` 只返回 `layoutRoot`。`TuiAltScreen`（`alt_screen.rs`）实现本 trait
/// 的同时把 [`Tui::is_viewport_tui`] 覆写为 `true`。
pub trait ViewportTui: Tui {
    /// 设置布局根（`setLayoutRoot(component)`；TS 的 `undefined` 对应 `None`）。
    fn set_layout_root(&mut self, component: Option<Box<dyn Component>>);
}

/// 一次 `dispatchMouseEvent` 调用的接收者记录（TS `result.target` 的组件 + 几何）。
///
/// TS 的 `TuiMouseDispatchResult.target.component` 是**最内层**产生结果的组件；Rust 的冻结
/// 结果类型不携带组件引用，备用屏渲染器（`alt_screen.rs`）用本记录在分发后还原它：
/// 每次 `dispatch_mouse_event` 返回结果时按**由内向外**的顺序压栈，最后一条是分发起点。
#[derive(Debug, Clone, Copy)]
pub(crate) struct MouseDispatchTrace {
    /// 接收者组件地址（`&mut dyn Component` 的数据指针）。
    pub address: usize,
    /// 该层的几何（若该层自己产生结果，就是 TS 的 `result.target`）。
    pub target: TuiMouseDispatchTarget,
}

thread_local! {
    /// 当前分发链（`Some` = 正在记录）。只由需要 target 链路的调用方开启。
    static MOUSE_DISPATCH_TRACE: RefCell<Option<Vec<MouseDispatchTrace>>> = const { RefCell::new(None) };
}

/// 开始记录一次最外层鼠标分发的接收者链（`alt_screen.rs` 用）。
pub(crate) fn begin_mouse_dispatch_trace() {
    MOUSE_DISPATCH_TRACE.with(|trace| *trace.borrow_mut() = Some(Vec::new()));
}

/// 结束记录并按**最内层优先**返回接收者链（未开启记录时为空）。
pub(crate) fn take_mouse_dispatch_trace() -> Vec<MouseDispatchTrace> {
    MOUSE_DISPATCH_TRACE.with(|trace| trace.borrow_mut().take().unwrap_or_default())
}

/// `dispatchMouseEvent`：把事件交给组件，并在命中时补全 target 与 focusTarget。
///
/// Rust 版不返回组件引用（见模块文档第 1 条偏差）：接收者记成它的组件地址
/// （[`MouseFocusTarget::Address`]），最外层分发者用 [`TuiBase::focus_target_of_address`]
/// 解析成 [`FocusTarget`]。TS 的 `"target" in result` 分支（组件已经转发给子组件）用
/// "结果里已经带 `Address`"来识别：转发型组件若有 `handleInput`（[`Component::handles_input`]）
/// 就把焦点收回给自己（`tui.ts:81-104`），否则保持命中子控件的地址。
pub fn dispatch_mouse_event(
    component: &mut dyn Component,
    event: &TuiMouseEvent,
) -> Option<TuiMouseEventResult> {
    let result = component.handle_mouse(event)?;
    // 没有请求焦点/捕获/处理的事件不产生分发结果（TS 同样不设置 target）。
    if !result.stops_propagation() {
        return None;
    }
    let address = component as *const dyn Component as *const () as usize;
    let forwarded = matches!(result.focus_target, Some(MouseFocusTarget::Address(_)));
    // focusTarget 的优先级：显式目标 > 转发结果里子组件的地址 > 当前接收者；未请求焦点
    // （且没有转发结果）时保持 `None`。
    let focus_target = match result.focus_target {
        // 显式目标：Rust 侧扩展，原样保留。
        Some(MouseFocusTarget::Explicit(target)) => Some(MouseFocusTarget::Explicit(target)),
        Some(MouseFocusTarget::Address(target)) if forwarded => {
            if result.focus == Some(true) && component.handles_input() {
                Some(MouseFocusTarget::Address(address))
            } else {
                Some(MouseFocusTarget::Address(target))
            }
        }
        _ if result.focus == Some(true) => Some(MouseFocusTarget::Address(address)),
        // TS 只在 `result.focus` 为真时写入 `focusTarget`。
        _ => None,
    };
    // 记录链路供备用屏重放捕获/按下目标：由内向外压栈，最后一条是分发起点。
    MOUSE_DISPATCH_TRACE.with(|trace| {
        if let Some(entries) = trace.borrow_mut().as_mut() {
            entries.push(MouseDispatchTrace {
                address,
                target: TuiMouseDispatchTarget {
                    origin_x: event.screen_x - event.x,
                    origin_y: event.screen_y - event.y,
                    width: event.width,
                    height: event.height,
                },
            });
        }
    });
    Some(TuiMouseEventResult {
        handled: Some(true),
        focus_target,
        ..result
    })
}

/// `retargetMouseEvent`：为之前命中的 target 重建局部坐标。
pub fn retarget_mouse_event(
    event: &TuiMouseEvent,
    target: &TuiMouseDispatchTarget,
) -> TuiMouseEvent {
    event.retarget(target)
}

// ============================================================================
// TuiBase：overlay / 焦点状态机 / 渲染调度 / 输入分发（TS `TuiBase`）
// ============================================================================

/// 焦点恢复策略（TS `OverlayFocusRestorePolicy`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayFocusRestorePolicy {
    /// `setFocus()`：显式设置焦点时清掉恢复状态。
    Clear,
    /// 输入处理里因 overlay 不可见而回退时保留。
    Preserve,
}

/// `OverlayBlockedFocusResume`：被阻塞的 overlay 焦点释放后如何恢复。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum OverlayBlockedFocusResume {
    /// 重新聚焦被阻塞的 overlay。
    RestoreOverlay,
    /// 交给显式目标。
    FocusTarget {
        #[serde(skip_serializing_if = "Option::is_none")]
        target: Option<FocusTarget>,
    },
}

/// 焦点恢复状态（TS `OverlayFocusRestoreState` 的四态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum OverlayFocusRestoreState {
    /// 没有待恢复的 overlay 焦点。
    #[default]
    Inactive,
    /// 当前聚焦的 overlay 可恢复。
    Eligible { overlay: usize },
    /// 焦点被非 overlay 组件抢占；`resume` 说明释放后如何恢复。
    Blocked {
        overlay: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        blocked_by: Option<FocusTarget>,
        resume: OverlayBlockedFocusResume,
    },
}

impl OverlayFocusRestoreState {
    /// 该状态涉及的 overlay id。
    pub fn overlay(&self) -> Option<usize> {
        match self {
            OverlayFocusRestoreState::Inactive => None,
            OverlayFocusRestoreState::Eligible { overlay }
            | OverlayFocusRestoreState::Blocked { overlay, .. } => Some(*overlay),
        }
    }
}

/// overlay 栈项（TS `OverlayStackEntry`；`component` 由 [`TuiCore`] 持有）。
pub struct OverlayEntry {
    /// 稳定 id（TS 用对象引用作身份）。
    pub id: usize,
    /// overlay 组件（TS `component`）。
    pub component: Box<dyn Component>,
    /// 展示时的选项（`None` == TS 的 `options === undefined`）。
    pub options: Option<OverlayOptions>,
    /// 打开前的焦点（TS `preFocus`）。
    pub pre_focus: Option<FocusTarget>,
    /// 临时隐藏标志。
    pub hidden: bool,
    /// 视觉顺序（越大越靠前）。
    pub focus_order: u64,
    /// 最近一次渲染的边界。
    pub bounds: Option<OverlayBounds>,
}

/// 已渲染 overlay 的布局（TS `RenderedOverlayLayout`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderedOverlayLayout {
    /// overlay id。
    pub id: usize,
    /// 相对终端左上角的单元格行（已向零取整）。
    pub row: usize,
    /// 相对终端左上角的单元格列（已向零取整）。
    pub col: usize,
    /// 单元格宽度。
    pub width: usize,
    /// 单元格高度（截断后的实际渲染行数）。
    pub height: usize,
}

/// overlay 与焦点的共享状态。
///
/// TS 里 `OverlayHandle` 是捕获 `this` 的闭包，`hide()` / `focus()` / `unfocus()` 都调用
/// `this.setFocus(...)`，由后者直接改**任何**组件的 `focused` 属性（`tui.ts:672-680`）。
/// Rust 的冻结签名 `show_overlay(&mut self) -> Box<dyn OverlayHandle>` 不允许 handle 借用
/// `TuiBase`，所以这段状态放进 `Rc<RefCell<TuiCore>>` 共享；handle 改完焦点后置脏位
/// （[`TuiCore::take_focus_dirty`]），由宿主下一次进入 [`TuiBase`] 时经
/// [`TuiBase::flush_pending_focus_sync`] 把 `focused` 写回真实组件。所有会改动焦点的
/// `TuiBase` 入口（`set_focus` / `show_overlay` / `hide_overlay` / `handle_terminal_input` /
/// [`TuiBase::pump_pending_input`]）都会先刷新，因此任何入口之后观察到的焦点标志都是最新的。
pub struct TuiCore {
    overlay_stack: Vec<OverlayEntry>,
    focus_order_counter: u64,
    next_overlay_id: usize,
    focused: Option<FocusTarget>,
    overlay_focus_restore: OverlayFocusRestoreState,
    rendered_overlay_layouts: Vec<RenderedOverlayLayout>,
    /// 挂载状态：`isComponentMounted`（TS `getMountedRoots()` + `containsComponent`）。
    ///
    /// `addresses` 是当前挂载树里**所有**组件的地址（顶层 + 递归进 `Container` 的子项，
    /// 以及子类 hook 追加的根，如 alt screen 的 `layoutRoot`）；`focus_addresses` 记录每个
    /// [`FocusTarget`] 最近一次被聚焦时对应组件的地址，用来复现 TS 的**引用相等**身份判定
    /// （`FocusTarget` 只是索引，子项被移除/替换后同一个索引可能指向另一个组件）。
    mount: Rc<RefCell<MountState>>,
    /// 终端尺寸的**实时**读取（`isOverlayVisible` 每次读 `terminal.columns/rows`）。
    terminal_size: Rc<dyn Fn() -> (usize, usize)>,
    /// 渲染请求标志（与 `TuiBase` 的调度器共享）。
    render_requested: Rc<std::cell::Cell<bool>>,
    /// `hideCursor()` 的等价物（`TuiBase` 注入）。
    hide_cursor: Rc<dyn Fn()>,
    /// 是否已 `stop()`（与 `TuiBase` 共享）：`hideTerminalCursor()` 只在运行期写终端。
    stopped: Rc<std::cell::Cell<bool>>,
    /// 焦点变更后尚未把 `focused` 标志刷到真实组件上（overlay handle 置位，
    /// `TuiBase::flush_pending_focus_sync` 消费）。
    focus_dirty: bool,
    /// 焦点代次：每次 `setFocusInternal` 自增。`TuiBase` 用它判断"脱离焦点"是否仍然有效
    /// （TS 的 `focusedComponent` 只在 `setFocus` 时被替换）。
    focus_epoch: u64,
}

/// 组件身份：`&dyn Component` 胖指针的（数据指针，vtable 指针）。
///
/// 只用数据指针无法区分"同一槽位换组件"（分配器可能复用同一地址），而 TS 的焦点恢复
/// 语义按组件引用比较（`blockedBy === previousFocus`、`isOverlayFocusAncestor`）。
type ComponentKey = (usize, usize);

/// 组件的（数据指针，vtable 指针）身份。
fn component_key(component: &dyn Component) -> ComponentKey {
    let pointer: *const dyn Component = component;
    // SAFETY: trait 对象指针的布局是 (data, vtable) 两个字；这里只读取指针值，不解引用。
    let (data, vtable): (*const (), *const ()) = unsafe { std::mem::transmute(pointer) };
    (data as usize, vtable as usize)
}

/// `TuiCore::mount` 的共享状态。
#[derive(Default)]
pub struct MountState {
    /// 当前挂载树中所有组件的身份（顶层 roots + 递归子项）。
    pub addresses: Vec<ComponentKey>,
    /// `FocusTarget` → 最近一次聚焦时该组件的身份。
    pub focus_addresses: Vec<(FocusTarget, ComponentKey)>,
}

impl MountState {
    fn address_of(&self, target: FocusTarget) -> Option<ComponentKey> {
        self.focus_addresses
            .iter()
            .rev()
            .find(|(candidate, _)| *candidate == target)
            .map(|(_, address)| *address)
    }

    fn remember(&mut self, target: FocusTarget, address: ComponentKey) {
        if let Some(entry) = self
            .focus_addresses
            .iter_mut()
            .rev()
            .find(|(candidate, _)| *candidate == target)
        {
            entry.1 = address;
            return;
        }
        self.focus_addresses.push((target, address));
    }

    fn is_address_mounted(&self, address: ComponentKey) -> bool {
        self.addresses.contains(&address)
    }
}

impl std::fmt::Debug for TuiCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TuiCore")
            .field("overlays", &self.overlay_stack.len())
            .field("focused", &self.focused)
            .field("restore", &self.overlay_focus_restore)
            .finish()
    }
}

impl TuiCore {
    fn new(
        terminal_size: Rc<dyn Fn() -> (usize, usize)>,
        render_requested: Rc<std::cell::Cell<bool>>,
        hide_cursor: Rc<dyn Fn()>,
        stopped: Rc<std::cell::Cell<bool>>,
        mount: Rc<RefCell<MountState>>,
    ) -> Self {
        TuiCore {
            overlay_stack: Vec::new(),
            focus_order_counter: 0,
            next_overlay_id: 0,
            focused: None,
            overlay_focus_restore: OverlayFocusRestoreState::Inactive,
            rendered_overlay_layouts: Vec::new(),
            mount,
            terminal_size,
            render_requested,
            hide_cursor,
            stopped,
            focus_dirty: false,
            focus_epoch: 0,
        }
    }

    /// 焦点变更后需要把 `focused` 标志刷到真实组件（见 [`TuiBase::flush_pending_focus_sync`]）。
    pub fn take_focus_dirty(&mut self) -> bool {
        std::mem::take(&mut self.focus_dirty)
    }

    /// `hasOverlayEntries`：栈非空（含隐藏项）。
    pub fn has_overlay_entries(&self) -> bool {
        !self.overlay_stack.is_empty()
    }

    /// 栈中 overlay 项数。
    pub fn overlay_count(&self) -> usize {
        self.overlay_stack.len()
    }

    /// overlay 栈（只读）。
    pub fn overlay_stack(&self) -> &[OverlayEntry] {
        &self.overlay_stack
    }

    /// 按 id 取出 overlay 组件（备用屏渲染器派发鼠标、重放 capture 目标时需要可变访问）。
    ///
    /// TS 的 `dispatchMouseToOverlay` / `dispatchMouseToTarget` 直接持有组件引用；
    /// Rust 的冻结结果类型不携带组件，渲染器用本方法在 overlay 栈里重新取回。
    pub fn overlay_component_mut(&mut self, id: usize) -> Option<&mut (dyn Component + 'static)> {
        let index = self.overlay_index(id)?;
        Some(self.overlay_stack[index].component.as_mut())
    }

    /// `hasOverlay()`：存在**可见** overlay。
    pub fn has_visible_overlay(&self) -> bool {
        self.overlay_stack
            .iter()
            .any(|entry| self.is_overlay_visible(entry))
    }

    /// `getFocusedComponent()`。
    pub fn focused(&self) -> Option<FocusTarget> {
        self.focused
    }

    /// 焦点代次：每次 [`TuiCore::set_focus_internal`] 自增；[`TuiBase`] 用它判断"已脱离
    /// 挂载树但仍有焦点"的子树是否仍然有效。
    pub fn focus_epoch(&self) -> u64 {
        self.focus_epoch
    }

    /// overlay 焦点恢复状态（只读；`TuiBase` 维护结构变化后的下标重映射时需要）。
    pub(crate) fn overlay_focus_restore_state(&self) -> OverlayFocusRestoreState {
        self.overlay_focus_restore
    }

    /// 把记录在案的所有焦点目标按 `mapping` 同时改指到同一组件的**新下标**。
    ///
    /// TS 的 `removeChild`/`clear` 不改 `focusedComponent`（引用不变）；Rust 的
    /// [`FocusTarget`] 是下标，删除前置兄弟后必须整体重映射（否则输入会投递给另一个组件，
    /// `blockedBy === previousFocus` 之类的**引用相等**判断也会失配）。映射一次性应用，
    /// 避免 `2→1` 之后再被 `1→0` 链式改写。这不触发 `setFocusInternal` 的任何副作用。
    pub(crate) fn apply_focus_target_mapping(&mut self, mapping: &[(FocusTarget, FocusTarget)]) {
        if mapping.is_empty() {
            return;
        }
        let lookup = |target: FocusTarget| -> Option<FocusTarget> {
            mapping
                .iter()
                .find(|(old, _)| *old == target)
                .map(|(_, new)| *new)
        };
        if let Some(current) = self.focused
            && let Some(new) = lookup(current)
        {
            self.focused = Some(new);
        }
        if let OverlayFocusRestoreState::Blocked {
            blocked_by, resume, ..
        } = &mut self.overlay_focus_restore
        {
            if let Some(current) = *blocked_by
                && let Some(new) = lookup(current)
            {
                *blocked_by = Some(new);
            }
            if let OverlayBlockedFocusResume::FocusTarget { target } = resume
                && let Some(current) = *target
                && let Some(new) = lookup(current)
            {
                *target = Some(new);
            }
        }
        for entry in &mut self.overlay_stack {
            if let Some(current) = entry.pre_focus
                && let Some(new) = lookup(current)
            {
                entry.pre_focus = Some(new);
            }
        }
        // 注意：重映射不是"重新设置焦点"（TS 的引用不变），因此不改 `focus_epoch`，
        // 已脱离挂载树的焦点（[`DetachedFocus`]）保持有效。
    }

    /// 已渲染的 overlay 布局（`dispatchMouseToOverlay` 用）。
    pub fn rendered_overlay_layouts(&self) -> &[RenderedOverlayLayout] {
        &self.rendered_overlay_layouts
    }

    /// 当前焦点恢复状态（测试/调试用）。
    pub fn focus_restore_state(&self) -> OverlayFocusRestoreState {
        self.overlay_focus_restore
    }

    /// 某个 overlay 的当前边界（`getBounds`）。
    pub fn overlay_bounds(&self, id: usize) -> Option<OverlayBounds> {
        let entry = self.overlay_stack.iter().find(|entry| entry.id == id)?;
        if !self.is_overlay_visible(entry) {
            return None;
        }
        entry.bounds
    }

    fn overlay_index(&self, id: usize) -> Option<usize> {
        self.overlay_stack.iter().position(|entry| entry.id == id)
    }

    /// `isOverlayVisible(entry)`——`visible()` 每次都用**当前**终端尺寸调用
    /// （TS `tui.ts:925-931` 传 `this.terminal.columns, this.terminal.rows`）。
    pub fn is_overlay_visible(&self, entry: &OverlayEntry) -> bool {
        if entry.hidden {
            return false;
        }
        match entry
            .options
            .as_ref()
            .and_then(|options| options.visible.as_ref())
        {
            Some(visible) => {
                let (columns, rows) = (self.terminal_size)();
                visible(columns, rows)
            }
            None => true,
        }
    }

    /// `getTopmostVisibleOverlay()`：跳过非捕获与不可见者，取 `focusOrder` 最大者。
    pub fn topmost_visible_overlay(&self) -> Option<usize> {
        let mut topmost: Option<&OverlayEntry> = None;
        for overlay in &self.overlay_stack {
            if overlay
                .options
                .as_ref()
                .and_then(|options| options.non_capturing)
                == Some(true)
                || !self.is_overlay_visible(overlay)
            {
                continue;
            }
            match topmost {
                Some(current) if overlay.focus_order <= current.focus_order => {}
                _ => topmost = Some(overlay),
            }
        }
        topmost.map(|entry| entry.id)
    }

    fn clear_overlay_focus_restore(&mut self) {
        self.overlay_focus_restore = OverlayFocusRestoreState::Inactive;
    }

    fn clear_overlay_focus_restore_for(&mut self, overlay: usize) {
        if self.overlay_focus_restore.overlay() == Some(overlay) {
            self.clear_overlay_focus_restore();
        }
    }

    /// 结算一个 `Blocked` 恢复状态并返回应恢复的焦点目标。
    ///
    /// `RestoreOverlay` → 回到被阻塞的 overlay；`FocusTarget` → 清除恢复状态并返回显式目标
    /// （target 本身可能是 `None`，表示清除焦点）。传入非 `Blocked` 状态时返回 `None`。
    fn resolve_blocked_overlay_focus_resume(
        &mut self,
        state: OverlayFocusRestoreState,
    ) -> Option<FocusTarget> {
        let OverlayFocusRestoreState::Blocked {
            overlay, resume, ..
        } = state
        else {
            return None;
        };
        match resume {
            OverlayBlockedFocusResume::RestoreOverlay => Some(FocusTarget::Overlay { id: overlay }),
            OverlayBlockedFocusResume::FocusTarget { target } => {
                self.clear_overlay_focus_restore();
                target
            }
        }
    }

    /// 把恢复状态按**当前可见性**过滤：目标 overlay 已被移除或不可见时视为 `Inactive`。
    ///
    /// TS 在输入处理与 `setFocusInternal` 里都会重新取可见的恢复状态，因此隐藏 overlay 或
    /// 终端尺寸变化导致 `visible()` 变假后，过期状态不会再把焦点拉回不可见的 overlay。
    fn visible_overlay_focus_restore(&self) -> OverlayFocusRestoreState {
        match self.overlay_focus_restore {
            OverlayFocusRestoreState::Inactive => OverlayFocusRestoreState::Inactive,
            state => match state.overlay().and_then(|id| self.overlay_index(id)) {
                Some(index) if self.is_overlay_visible(&self.overlay_stack[index]) => state,
                _ => OverlayFocusRestoreState::Inactive,
            },
        }
    }

    /// `isOverlayFocusAncestor(entry, component)`：沿 `preFocus` 链上溯（带环检测）。
    ///
    /// 返回 true 表示 `target` 位于该 overlay 的 `preFocus` 链上，即"把焦点移到这里不算被
    /// 别的组件抢走"。链上的位置目标（`Component`/`Path`/`LayoutRoot`）还要核对
    /// [`MountState`]：同一槽位可能已经换成另一个组件，此时按 TS 的引用比较应返回 false。
    /// overlay 链遇到环时返回 false，避免 `preFocus` 互相引用导致死循环。
    fn is_overlay_focus_ancestor(&self, entry: usize, target: Option<FocusTarget>) -> bool {
        let mut visited: Vec<usize> = Vec::new();
        let mut current = self
            .overlay_index(entry)
            .and_then(|index| self.overlay_stack[index].pre_focus);
        while let Some(candidate) = current {
            match candidate {
                FocusTarget::Overlay { id } => {
                    if visited.contains(&id) {
                        return false;
                    }
                    visited.push(id);
                    if Some(candidate) == target {
                        return true;
                    }
                    current = self
                        .overlay_index(id)
                        .and_then(|index| self.overlay_stack[index].pre_focus);
                }
                FocusTarget::Component { .. }
                | FocusTarget::Path { .. }
                | FocusTarget::LayoutRoot { .. } => {
                    if Some(candidate) != target {
                        return false;
                    }
                    // 位置目标可能被**另一个组件**复用（例如编辑器槽里把编辑器换成扩展
                    // 对话框/选择器：下标相同、组件不同）。TS 比较组件引用；这里用"聚焦该
                    // 目标时记录的组件地址是否仍在挂载树里"复现同一判定。
                    let mount = self.mount.borrow();
                    return match mount.address_of(candidate) {
                        Some(address) => mount.is_address_mounted(address),
                        None => true,
                    };
                }
            }
        }
        false
    }

    /// `isComponentMounted(component)`：`getMountedRoots().some(root => containsComponent(root, c))`。
    ///
    /// 组件目标按**记录地址**（[`MountState`]）判定身份，复现 TS 的引用相等语义：子项被移除后
    /// 同一个下标可能指向另一个组件，那不算"仍然挂载"。overlay 目标看它是否还在栈里。
    fn is_component_mounted(&self, target: Option<FocusTarget>) -> bool {
        match target {
            None => false,
            Some(
                target @ (FocusTarget::Component { .. }
                | FocusTarget::Path { .. }
                | FocusTarget::LayoutRoot { .. }),
            ) => {
                let mount = self.mount.borrow();
                match mount.address_of(target) {
                    Some(address) => mount.is_address_mounted(address),
                    // 从未经 `TuiBase::set_focus` 记录过（例如直接构造的目标）：退回存在性判定。
                    None => true,
                }
            }
            Some(FocusTarget::Overlay { id }) => self.overlay_index(id).is_some(),
        }
    }

    /// `retargetOverlayPreFocus(removed)`。
    fn retarget_overlay_pre_focus(&mut self, removed: usize) {
        let removed_pre_focus = self
            .overlay_index(removed)
            .and_then(|index| self.overlay_stack[index].pre_focus);
        for overlay in &mut self.overlay_stack {
            if overlay.id != removed
                && overlay.pre_focus == Some(FocusTarget::Overlay { id: removed })
            {
                overlay.pre_focus = removed_pre_focus;
            }
        }
    }

    /// `setFocusInternal({component, overlayFocusRestore})`——逐分支对应 TS。
    ///
    /// 焦点恢复状态（`overlay_focus_restore`）只有三种：
    /// - `Inactive`：没有待恢复的 overlay 焦点；
    /// - `Eligible { overlay }`：焦点停在一个可见 overlay 上，离开它可以被恢复；
    /// - `Blocked { overlay, blocked_by, resume }`：焦点被其他组件抢走，`resume` 说明释放
    ///   （例如 `unfocus`）后回到该 overlay 还是交给显式目标。
    ///
    /// 决策顺序：
    /// 1. 先按**当前可见性**过滤恢复状态（目标 overlay 不在栈里或不可见 → 视为 `Inactive`）；
    /// 2. 新目标是普通组件时，如果旧状态是 `Blocked` 且 `blocked_by` 正是当前焦点：
    ///    `resume` 为显式目标或 `blocked_by` 已不在挂载树 → 结算（可能把焦点改回 overlay）；
    ///    否则只把 `blocked_by` 更新为新目标；
    /// 3. 否则，若焦点刚从可见 overlay 离开、且新目标不在该 overlay 的 `preFocus` 链上，
    ///    记为 `Blocked { resume: RestoreOverlay }`；
    /// 4. 新目标为 `None` 时：`Blocked` 且 `blocked_by` 是当前焦点 → 结算；否则只有
    ///    `Clear` 策略会清除恢复状态（`Preserve` 留给输入处理里的临时回退）；
    /// 5. 写入 `focused`；若焦点落在**可见** overlay 上，状态改为 `Eligible`；
    /// 6. 置 `focus_dirty`（真实组件标志由 [`TuiBase::flush_pending_focus_sync`] 刷新）并
    ///    自增 [`TuiCore::focus_epoch`]。
    ///
    /// 注意：判断"新目标是 overlay"只看 id 是否还在栈里，**不检查可见性**；聚焦一个存在但
    /// 不可见的 overlay 会跳过上面的 Blocked 逻辑，且不会进入 `Eligible`。
    pub fn set_focus_internal(
        &mut self,
        target: Option<FocusTarget>,
        policy: OverlayFocusRestorePolicy,
    ) {
        let previous_focus = self.focused;
        let mut next_focus = target;
        let previous_focused_overlay = match previous_focus {
            Some(FocusTarget::Overlay { id }) => self
                .overlay_index(id)
                .filter(|&index| self.is_overlay_visible(&self.overlay_stack[index]))
                .map(|_| id),
            _ => None,
        };
        let next_focus_is_overlay = match next_focus {
            Some(FocusTarget::Overlay { id }) => self.overlay_index(id).is_some(),
            _ => false,
        };
        let restore_state = self.visible_overlay_focus_restore();

        if next_focus.is_some() && !next_focus_is_overlay {
            let blocked_state = match restore_state {
                OverlayFocusRestoreState::Blocked { blocked_by, .. }
                    if blocked_by == previous_focus =>
                {
                    Some(restore_state)
                }
                _ => None,
            };
            if let Some(blocked) = blocked_state {
                let OverlayFocusRestoreState::Blocked {
                    overlay,
                    blocked_by,
                    resume,
                } = blocked
                else {
                    unreachable!("blocked_state 只可能是 Blocked");
                };
                let resume_is_target =
                    matches!(resume, OverlayBlockedFocusResume::FocusTarget { .. });
                if resume_is_target || !self.is_component_mounted(blocked_by) {
                    next_focus = self.resolve_blocked_overlay_focus_resume(blocked);
                } else {
                    self.overlay_focus_restore = OverlayFocusRestoreState::Blocked {
                        overlay,
                        blocked_by: next_focus,
                        resume,
                    };
                }
            } else if let Some(previous_overlay) = previous_focused_overlay
                && !matches!(restore_state, OverlayFocusRestoreState::Inactive)
                && restore_state.overlay() == Some(previous_overlay)
                && !self.is_overlay_focus_ancestor(previous_overlay, next_focus)
            {
                self.overlay_focus_restore = OverlayFocusRestoreState::Blocked {
                    overlay: previous_overlay,
                    blocked_by: next_focus,
                    resume: OverlayBlockedFocusResume::RestoreOverlay,
                };
            }
        } else if next_focus.is_none() {
            let blocked_by_previous = matches!(
                restore_state,
                OverlayFocusRestoreState::Blocked { blocked_by, .. } if blocked_by == previous_focus
            );
            if blocked_by_previous {
                next_focus = self.resolve_blocked_overlay_focus_resume(restore_state);
            } else if policy == OverlayFocusRestorePolicy::Clear {
                self.clear_overlay_focus_restore();
            }
        }

        self.focused = next_focus;

        let focused_overlay = match next_focus {
            Some(FocusTarget::Overlay { id }) => self
                .overlay_index(id)
                .filter(|&index| self.is_overlay_visible(&self.overlay_stack[index]))
                .map(|_| id),
            _ => None,
        };
        if let Some(overlay) = focused_overlay {
            self.overlay_focus_restore = OverlayFocusRestoreState::Eligible { overlay };
        }
        // TS 在 `setFocusInternal` 里直接改新旧组件的 `focused` 属性；Rust 的 `TuiBase` 持有
        // 真实组件，无法从这里访问，因此置脏由 [`TuiBase::flush_pending_focus_sync`] 统一刷新。
        self.focus_dirty = true;
        self.focus_epoch = self.focus_epoch.wrapping_add(1);
    }

    /// 把 `focused` 标志写到 overlay 组件（TS 在 `setFocusInternal` 里直接改 `focused` 属性）。
    fn apply_focus_flags_to_overlays(&mut self) {
        for entry in &mut self.overlay_stack {
            let should_focus = self.focused == Some(FocusTarget::Overlay { id: entry.id });
            if let Some(focusable) = entry.component.as_focusable() {
                focusable.set_focused(should_focus);
            }
        }
    }

    /// `setFocus(component)`：按 `Clear` 策略设置焦点，并立即把 `focused` 标志写到 overlay
    /// 组件；挂载树里的真实组件由 [`TuiBase::flush_pending_focus_sync`] 刷新。
    pub fn set_focus(&mut self, target: Option<FocusTarget>) {
        self.set_focus_internal(target, OverlayFocusRestorePolicy::Clear);
        self.apply_focus_flags_to_overlays();
    }

    /// `isOverlayFocused()`。
    pub fn is_overlay_focused(&self) -> bool {
        self.overlay_stack.iter().any(|entry| {
            self.focused == Some(FocusTarget::Overlay { id: entry.id })
                && self.is_overlay_visible(entry)
        })
    }

    /// `resolveMouseFocusTarget(component)`。
    ///
    /// TS 用 `containsComponent` 判断命中组件是否在某个 overlay 内；Rust 侧组件不可内省，
    /// 因此这里只在目标本身就是 overlay 组件时保持它（`TuiAltScreen` 负责先做盒→组件映射）。
    pub fn resolve_mouse_focus_target(&self, target: FocusTarget) -> FocusTarget {
        if let Some(entry) = self.overlay_stack.iter().rev().find(|entry| {
            self.is_overlay_visible(entry)
                && Some(FocusTarget::Overlay { id: entry.id }) == Some(target)
        }) {
            return FocusTarget::Overlay { id: entry.id };
        }
        target
    }

    fn set_rendered_overlay_layouts(&mut self, layouts: Vec<RenderedOverlayLayout>) {
        self.rendered_overlay_layouts = layouts;
    }

    fn reset_overlay_bounds(&mut self) {
        for entry in &mut self.overlay_stack {
            entry.bounds = None;
        }
    }

    fn push_overlay(
        &mut self,
        component: Box<dyn Component>,
        options: Option<OverlayOptions>,
    ) -> usize {
        self.focus_order_counter += 1;
        self.next_overlay_id += 1;
        let id = self.next_overlay_id;
        self.overlay_stack.push(OverlayEntry {
            id,
            component,
            options,
            pre_focus: self.focused,
            hidden: false,
            focus_order: self.focus_order_counter,
            bounds: None,
        });
        id
    }

    fn bump_focus_order(&mut self, id: usize) {
        self.focus_order_counter += 1;
        if let Some(index) = self.overlay_index(id) {
            self.overlay_stack[index].focus_order = self.focus_order_counter;
        }
    }

    fn remove_overlay(&mut self, id: usize) -> Option<OverlayEntry> {
        let index = self.overlay_index(id)?;
        let mut entry = self.overlay_stack.remove(index);
        // TS 的 `setFocusInternal` 会先把**旧的** `focusedComponent` 置为 false；移除之后
        // 那个组件已不在栈里，`apply_focus_flags_to_overlays` 再也看不到它，因此在移除处补上
        // （否则被移除的焦点 overlay 会永远保留 `focused == true`）。
        if self.focused == Some(FocusTarget::Overlay { id })
            && let Some(focusable) = entry.component.as_focusable()
        {
            focusable.set_focused(false);
        }
        Some(entry)
    }

    fn request_render(&self) {
        self.render_requested.set(true);
    }

    /// `hideTerminalCursor()`：`if (!this.stopped) this.terminal.hideCursor()`。
    ///
    /// `stop()` 之后 shell 接管光标，必须保持可见（TS 的 issue #10026：`stop()` 后再
    /// `hideOverlay()`/`handle.hide()` 不得再隐藏光标）。判定放在这里，`TuiBase` 与
    /// overlay handle 两条路径都经过它。
    fn hide_terminal_cursor(&self) {
        if !self.stopped.get() {
            (self.hide_cursor)();
        }
    }
}

/// `showOverlay` 返回的句柄（TS 里捕获 `this` 的闭包对象）。
pub struct TuiOverlayHandle {
    core: Rc<RefCell<TuiCore>>,
    id: usize,
}

impl std::fmt::Debug for TuiOverlayHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TuiOverlayHandle")
            .field("id", &self.id)
            .finish()
    }
}

impl TuiOverlayHandle {
    fn exists(core: &TuiCore, id: usize) -> bool {
        core.overlay_index(id).is_some()
    }

    fn visible(core: &TuiCore, id: usize) -> bool {
        core.overlay_index(id)
            .is_some_and(|index| core.is_overlay_visible(&core.overlay_stack[index]))
    }
}

impl OverlayHandle for TuiOverlayHandle {
    fn hide(&mut self) {
        let mut core = self.core.borrow_mut();
        let id = self.id;
        if core.overlay_index(id).is_none() {
            return;
        }
        // 顺序与 TuiBase::hide_overlay 一致：先清本 overlay 的恢复状态与其它 overlay 对它的
        // `preFocus` 引用，再移除；若它正是焦点，交给最上层可见的捕获型 overlay
        // （topmost_visible_overlay 跳过 non_capturing），否则回退 preFocus。
        core.clear_overlay_focus_restore_for(id);
        core.retarget_overlay_pre_focus(id);
        let Some(entry) = core.remove_overlay(id) else {
            return;
        };
        if core.focused == Some(FocusTarget::Overlay { id }) {
            let next = core
                .topmost_visible_overlay()
                .map(|overlay| FocusTarget::Overlay { id: overlay })
                .or(entry.pre_focus);
            core.set_focus(next);
        }
        if core.overlay_stack.is_empty() {
            core.hide_terminal_cursor();
        }
        core.request_render();
    }

    fn set_hidden(&mut self, hidden: bool) {
        let mut core = self.core.borrow_mut();
        let id = self.id;
        let Some(index) = core.overlay_index(id) else {
            return;
        };
        if core.overlay_stack[index].hidden == hidden {
            return;
        }
        core.overlay_stack[index].hidden = hidden;
        let non_capturing = core.overlay_stack[index]
            .options
            .as_ref()
            .and_then(|options| options.non_capturing)
            == Some(true);
        if hidden {
            // 隐藏时先丢弃它的恢复状态；若它正持有焦点，改投最上层可见的捕获型 overlay
            // （topmost_visible_overlay 跳过 non_capturing）或 preFocus。
            core.clear_overlay_focus_restore_for(id);
            if core.focused == Some(FocusTarget::Overlay { id }) {
                let pre_focus = core.overlay_stack[index].pre_focus;
                let next = core
                    .topmost_visible_overlay()
                    .map(|overlay| FocusTarget::Overlay { id: overlay })
                    .or(pre_focus);
                core.set_focus(next);
            }
        } else if !non_capturing && TuiOverlayHandle::visible(&core, id) {
            // 重新显示：只有可见的捕获型 overlay 才抢焦点并置前；non-capturing 保持现状。
            core.bump_focus_order(id);
            core.set_focus(Some(FocusTarget::Overlay { id }));
        }
        core.request_render();
    }

    fn is_hidden(&self) -> bool {
        let core = self.core.borrow();
        core.overlay_index(self.id)
            .is_some_and(|index| core.overlay_stack[index].hidden)
    }

    fn focus(&mut self) {
        let mut core = self.core.borrow_mut();
        let id = self.id;
        if !TuiOverlayHandle::exists(&core, id) || !TuiOverlayHandle::visible(&core, id) {
            return;
        }
        core.bump_focus_order(id);
        core.set_focus(Some(FocusTarget::Overlay { id }));
        core.request_render();
    }

    fn unfocus(&mut self, options: Option<OverlayUnfocusOptions>) {
        let mut core = self.core.borrow_mut();
        let id = self.id;
        let is_focused = core.focused == Some(FocusTarget::Overlay { id });
        let restore_state = core.overlay_focus_restore;
        let has_pending_restore = restore_state.overlay() == Some(id);
        if !is_focused && !has_pending_restore {
            return;
        }
        // 焦点已被别的组件抢走（Blocked）：此时只改写恢复策略，不立即改焦点；给出的
        // target 可能为 None（表示将来清除焦点）。没有显式选项则直接清掉恢复状态。
        if let OverlayFocusRestoreState::Blocked {
            overlay,
            blocked_by,
            ..
        } = restore_state
            && overlay == id
            && core.focused == blocked_by
        {
            if let Some(unfocus_options) = options {
                core.overlay_focus_restore = OverlayFocusRestoreState::Blocked {
                    overlay: id,
                    blocked_by,
                    resume: OverlayBlockedFocusResume::FocusTarget {
                        target: unfocus_options.target,
                    },
                };
            } else {
                core.clear_overlay_focus_restore();
            }
            core.request_render();
            return;
        }
        core.clear_overlay_focus_restore_for(id);
        // 正常释放：显式 target 优先；否则交回最上层可见的捕获型 overlay
        // （topmost_visible_overlay 跳过 non_capturing，并排除自己），没有则回到
        // 打开前的焦点。`is_focused || options.is_some()` 保证"仅 pending restore"时不误改焦点。
        if is_focused || options.is_some() {
            let pre_focus = core
                .overlay_index(id)
                .and_then(|index| core.overlay_stack[index].pre_focus);
            let fallback = match core.topmost_visible_overlay() {
                Some(overlay) if overlay != id => Some(FocusTarget::Overlay { id: overlay }),
                _ => pre_focus,
            };
            let next = match options {
                Some(unfocus_options) => unfocus_options.target,
                None => fallback,
            };
            core.set_focus(next);
        }
        core.request_render();
    }

    fn is_focused(&self) -> bool {
        self.core.borrow().focused == Some(FocusTarget::Overlay { id: self.id })
    }

    fn bounds(&self) -> Option<OverlayBounds> {
        self.core.borrow().overlay_bounds(self.id)
    }
}

/// 定时器抽象（`spec/tui-core.md` §8.2 第 5 条）：生产实现可用 tokio，测试用可手动触发的替身。
pub trait Timer {
    /// 在 `delay_ms` 毫秒后调用一次 `callback`。
    fn set(&mut self, delay_ms: u64, callback: Box<dyn FnOnce()>);
    /// 取消未触发的回调。
    fn cancel(&mut self);
}

/// 默认定时器：不调度（由宿主事件循环按 [`RenderDecision`] 驱动）。
#[derive(Debug, Default)]
pub struct NoopTimer;

impl Timer for NoopTimer {
    fn set(&mut self, _delay_ms: u64, _callback: Box<dyn FnOnce()>) {}
    fn cancel(&mut self) {}
}

/// 渲染调度决策（[`TuiBase::render_decision`]）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RenderDecision {
    /// 不需要渲染。
    Idle,
    /// 立刻渲染。
    Now,
    /// 需要渲染，但应等待 `remaining_ms` 毫秒（`MIN_RENDER_INTERVAL_MS` 节流）。
    Wait(f64),
}

/// 终端颜色查询的目标键（TS `String(target)`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum TerminalColorTargetKey {
    Foreground,
    Background,
    Palette(u16),
}

struct PendingTerminalColorQuery {
    /// 查询 id（并发查询按 id 精确定位，超时不会误伤别的查询）。
    id: usize,
    foreground: Option<RgbColor>,
    background: Option<RgbColor>,
    palette: Vec<Option<RgbColor>>,
    replied: std::collections::HashSet<TerminalColorTargetKey>,
    /// 结果投递端：`queryTerminalColors` 的 promise resolve（超时后置空）。
    deliver: Option<tokio::sync::oneshot::Sender<TerminalColors>>,
    /// 超时后的迟到回复接收者（TS `query.deliver = onLateReply`）。
    ///
    /// **调用时**就存进来（TS 在超时回调里才赋值，但语义等价：正常完成时丢弃它，
    /// 见 [`complete_terminal_color_query`]）。`deliver` 与 `late` 都为 `None` 时，后续颜色
    /// 回复被**消费但忽略**（TS `if (!query.deliver …) return true`）。
    late: Option<Box<dyn FnMut(TerminalColors)>>,
    /// 截止时刻：TS 的 `setTimeout` 在 `queryTerminalColors` 调用时创建，计时从**调用**开始
    /// （`tui.ts:1520-1542`），而不是从 promise 第一次被 await 开始。
    deadline: tokio::time::Instant,
    /// 截止时刻已过时 `resolve` 用的部分结果快照：在第一条迟到回复到达前记录，之后到达的
    /// 回复只进 `late`（TS 在 `setTimeout` 触发时 `resolve(terminalColorQueryResult(query))`）。
    ///
    /// 与返回的 future **共享**：TS 的 promise 在 `setTimeout` 触发时就已经 resolve，哪怕
    /// 查询项此后被 DA1 移出队列，结果也不会变；future 因此必须在查询项消失后仍能拿到快照。
    snapshot: Rc<RefCell<Option<TerminalColors>>>,
}

/// `completeTerminalColorQuery(query)`：把结果交给 `deliver`（promise resolve），超时后交给
/// `late`（`onLateReply`），并清空两者——与 TS 先 `query.deliver = undefined` 再调用的语义一致。
fn complete_terminal_color_query(query: &mut PendingTerminalColorQuery) {
    let result = query.result();
    if let Some(deliver) = query.deliver.take() {
        // 正常完成（截止前收到 DA1/收齐 16 色）：TS 从未安装 `onLateReply`（它只在超时回调里
        // `query.deliver = onLateReply`），之后到达的回复必须被消费但忽略。
        query.late = None;
        let _ = deliver.send(result);
    } else if let Some(mut late) = query.late.take() {
        late(result);
    }
}

impl PendingTerminalColorQuery {
    /// 截止时刻是否已过（TS 的 `setTimeout` 是否已经触发）。
    fn deadline_passed(&self) -> bool {
        tokio::time::Instant::now() >= self.deadline
    }

    /// 若截止时刻已过而 promise 尚未 resolve，补做 TS `setTimeout` 回调的状态切换：
    /// 记录此刻的部分结果快照，并丢弃投递端（让 future 走超时分支返回快照）。
    fn timeout_if_due(&mut self) {
        if self.deliver.is_none() || !self.deadline_passed() {
            return;
        }
        let snapshot = self.result();
        *self.snapshot.borrow_mut() = Some(snapshot);
        self.deliver = None;
    }

    /// 组装当前结果：只有 16 个调色板槽位**全部**收到时才给出 `palette`，否则为 `None`
    /// （TS 的 `palette.every(...)` 判定）。
    fn result(&self) -> TerminalColors {
        let palette = if self.palette.iter().all(Option::is_some) {
            Some(self.palette.iter().filter_map(|color| *color).collect())
        } else {
            None
        };
        TerminalColors {
            foreground: self.foreground,
            background: self.background,
            palette,
        }
    }
}

/// 终端明暗方案变化监听器（TS `onTerminalColorSchemeChange` 的回调）。
pub type TerminalColorSchemeListener = (usize, Box<dyn FnMut(TerminalColorScheme)>);

/// `TuiBase` 的 protected 覆写点（TS 里由子类 `override` 的那些方法）。
///
/// TS 的 `TuiAltScreen` / `TuiMainScreen` 通过继承覆写下列钩子；Rust 侧 `TuiBase` 是被持有
/// 的结构体，因此改由宿主注入一个实现本 trait 的对象（[`TuiBase::set_lifecycle_hooks`]）。
/// 默认实现逐条对应 `tui.ts` 的空方法 / 固定返回值。
pub trait TuiLifecycleHooks {
    /// `resetRenderState()`：全量重绘前清空渲染器缓存。
    fn reset_render_state(&mut self) {}
    /// `beforeTerminalStart()`：在 `terminal.start` 之前。
    fn before_terminal_start(&mut self) {}
    /// `afterTerminalStart()`：在 `terminal.start` 之后、`hideCursor` 之前。
    fn after_terminal_start(&mut self) {}
    /// `beforeTerminalStop(options)`：在 `terminal.showCursor` 之前。
    fn before_terminal_stop(&mut self, _options: TuiStopOptions) {}
    /// `afterTerminalStop(options)`：在 `terminal.stop` 之后。
    fn after_terminal_stop(&mut self, _options: TuiStopOptions) {}
    /// `getExitRestorePrefix()`：退出恢复的第一段。
    ///
    /// `base` 是 `TuiBase` 的默认实现（`?2031l` 视通知开关 + `?25h`），即基类的退出恢复前缀；
    /// `tui-alt-screen.ts:431-436` 在它前面拼自己的模式重置。
    fn exit_restore_prefix(&mut self, base: &str) -> String {
        base.to_string()
    }
    /// `getExitRestoreSuffix()`：退出恢复的最后一段（同样接收 `super.…` 的结果）。
    fn exit_restore_suffix(&mut self, base: &str) -> String {
        base.to_string()
    }
}

/// 默认时钟：相对进程启动的单调毫秒（`performance.now()` 的等价物，`spec/tui-core.md` §8.2）。
pub fn monotonic_clock() -> f64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    let start = *START.get_or_init(Instant::now);
    start.elapsed().as_secs_f64() * 1000.0
}

/// 递归收集组件地址（`containsComponent(root, target)` 的等价物，见 [`MountState`]）。
///
/// 用 [`Component::for_each_container_child`] 下钻（TS 的 `instanceof Container`），因此能穿过
/// `Stack`/`ScrollView` 这类包装 `Container` 的组件（含它们的共享句柄代理，代理会把遍历
/// 透传给底层容器，见 [`Component::for_each_container_child`] 的文档）。
fn collect_component_addresses(component: &dyn Component, out: &mut Vec<ComponentKey>) {
    out.push(component_key(component));
    component.for_each_container_child(&mut |child| {
        collect_component_addresses(child, out);
        true
    });
}

/// 按从上到下的下标路径找到组件地址；空路径返回 `root` 自己的地址。
fn component_path_to_address(component: &dyn Component, address: usize) -> Option<Vec<usize>> {
    if component as *const dyn Component as *const () as usize == address {
        return Some(Vec::new());
    }
    let mut found = None;
    let mut index = 0usize;
    component.for_each_container_child(&mut |child| {
        if let Some(mut path) = component_path_to_address(child, address) {
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

/// 沿下标路径找到组件地址（空路径 = `component` 自己）。
fn component_key_at_path(component: &dyn Component, indices: &[u32]) -> Option<ComponentKey> {
    let Some((first, rest)) = indices.split_first() else {
        return Some(component_key(component));
    };
    let index = usize::try_from(*first).ok()?;
    let mut found = None;
    let mut child_index = 0usize;
    component.for_each_container_child(&mut |child| {
        if child_index == index {
            found = component_key_at_path(child, rest);
            return false;
        }
        child_index += 1;
        true
    });
    found
}

/// 把构造出的下标序列转成 [`FocusPath`]（空序列 → [`FocusPath::root`]）。
fn focus_path_from(indices: Vec<usize>) -> FocusPath {
    FocusPath::new(&indices).unwrap_or_else(FocusPath::root)
}

/// 沿下标路径对末端组件执行回调（空路径 = `component` 自己）。
///
/// 用回调而不是返回 `&mut`，是因为共享句柄代理（`Stack`/`ScrollView` 的子项）只能通过
/// [`Component::for_each_container_child_mut`] 借出底层子项，引用无法越过借用守卫逃逸。
fn with_component_mut_at_path(
    component: &mut (dyn Component + 'static),
    indices: &[u32],
    f: &mut dyn FnMut(&mut (dyn Component + 'static)),
) {
    let Some((first, rest)) = indices.split_first() else {
        f(component);
        return;
    };
    let index = usize::try_from(*first).ok();
    let mut position = 0usize;
    component.for_each_container_child_mut(&mut |child| {
        if Some(position) == index {
            with_component_mut_at_path(child, rest, f);
            false
        } else {
            position += 1;
            true
        }
    });
}

/// 递归清除组件树上的 `focused` 标志。
///
/// TS 的 `setFocusInternal` 只把**旧的**聚焦组件置 false；这里先把整棵挂载树清零，再给新目标
/// 置 true，可观察结果相同（只有 TUI 会写这个标志），且能处理嵌套目标之间的切换。
fn clear_focus_flags(component: &mut dyn Component) {
    if let Some(focusable) = component.as_focusable() {
        focusable.set_focused(false);
    }
    component.for_each_container_child_mut(&mut |child| {
        clear_focus_flags(child);
        true
    });
}

/// 沿下标路径把 `focused` 标志写到末端组件（空路径 = `component` 自己）。
fn set_focus_flag_at(component: &mut dyn Component, indices: &[u32]) {
    let Some((first, rest)) = indices.split_first() else {
        if let Some(focusable) = component.as_focusable() {
            focusable.set_focused(true);
        }
        return;
    };
    let index = usize::try_from(*first).ok();
    let mut position = 0usize;
    component.for_each_container_child_mut(&mut |child| {
        if Some(position) == index {
            set_focus_flag_at(child, rest);
            false
        } else {
            position += 1;
            true
        }
    });
}

/// "已脱离挂载树但仍持有焦点"的子树（TS `focusedComponent` 的引用语义）。
///
/// TS 的 `removeChild` / `clear` 只改 `children` 数组，`focusedComponent` 仍指向被删组件：
/// 后续 `handleTerminalInput` 继续投递给它，`focused` 标志也保持不变。Rust 的
/// [`FocusTarget`] 是下标/路径，无法直接表示"不在树里的对象"，因此删除聚焦子树时把它
/// 原样保留在这里，输入分发与标志刷新都以它为准（见 [`TuiBase::reconcile_focus_after_removal`]）。
struct DetachedFocus {
    /// 被移出挂载树的子树（TS 里仍被外部引用，因此不会被回收）。
    component: Box<dyn Component>,
    /// 焦点在该子树里的下标路径（空 = 子树根）。
    path: Vec<u32>,
    /// 脱离时的焦点代次；焦点一旦被重新设置（[`TuiCore::focus_epoch`] 改变）即失效。
    epoch: u64,
}

/// TUI 基类（TS `TuiBase`）：overlay 栈、焦点状态机、渲染调度与输入分发。
///
/// 具体渲染器（`TuiMainScreen` / `TuiAltScreen`）持有本结构，在自己的 `doRender` 里调用
/// [`TuiBase::composite_overlays`]、[`TuiBase::apply_line_resets`]、
/// [`TuiBase::extract_cursor_position`]、[`TuiBase::resolve_fake_cursors`]，并实现 `Tui` trait。
pub struct TuiBase {
    /// 终端（与 overlay handle 共享）。
    pub terminal: Rc<RefCell<Box<dyn Terminal>>>,
    /// 顶层子组件（TS `children`）。
    pub children: Vec<Box<dyn Component>>,
    /// 布局根（TS `TuiAltScreen.layoutRoot`）。设置后它**取代** `children` 成为唯一的挂载根，
    /// 与 `tui-alt-screen.ts:339` 的 `getMountedRoots()` 一致；由 [`TuiBase::set_layout_root`] 维护。
    /// 主屏 TUI 永远不设置它。
    pub layout_root: Option<Box<dyn Component>>,
    /// 生命周期钩子（TS 的 `beforeTerminalStart` 等 protected 覆写点）。
    hooks: Option<Rc<RefCell<Box<dyn TuiLifecycleHooks>>>>,
    /// 退出恢复处理器是否已注册（对应 TS `process.on("exit", …)` 的挂/摘）。
    exit_restore_registered: bool,
    /// overlay 与焦点共享状态。
    core: Rc<RefCell<TuiCore>>,
    /// 调试键（Shift+Ctrl+D）回调。
    pub on_debug: Option<Box<dyn FnMut()>>,
    /// 硬件光标开关（`setShowHardwareCursor`）。
    pub show_hardware_cursor: bool,
    /// 内容收缩时是否清空多余行。
    pub clear_on_shrink: bool,
    /// 累计全量重绘次数（`fullRedraws`）。
    pub full_redraw_count: u64,
    /// 是否已 `stop()`。
    ///
    /// `TuiCore`（overlay/焦点状态机）也读它来决定 `hideTerminalCursor()` 是否真的写终端
    /// （TS `tui.ts:868` 的 `if (!this.stopped) this.terminal.hideCursor()`），因此经
    /// [`TuiBase::core`] 与具体渲染器共享。
    pub stopped: Rc<std::cell::Cell<bool>>,
    /// 调试/崩溃日志目录。
    pub log_directory: Option<String>,
    /// 终端明暗方案通知是否开启。
    pub terminal_color_scheme_notifications_enabled: bool,
    input_listeners: Vec<(InputListenerId, TuiInputListener)>,
    next_listener_id: usize,
    terminal_color_scheme_listeners: Vec<TerminalColorSchemeListener>,
    next_scheme_listener_id: usize,
    /// 未决颜色查询（与 [`TuiBase::query_terminal_colors_with_late_reply`] 返回的 future 共享）。
    pending_terminal_color_queries: Rc<RefCell<Vec<PendingTerminalColorQuery>>>,
    next_color_query_id: usize,
    render_requested: Rc<std::cell::Cell<bool>>,
    immediate_render_scheduled: bool,
    last_render_at: f64,
    timer: Box<dyn Timer>,
    clock: Box<dyn Fn() -> f64>,
    /// `setCellDimensions` 的默认接收方（TS `consumeCellSizeResponse` 无条件调用的
    /// `terminal-image.ts` 里的 `setCellDimensions`）。
    ///
    /// TS 的 `TuiBase` 直接 import 该函数；Rust 侧保留注入点是为了便于测试替换，但**默认
    /// 已指向 `terminal_image::set_cell_dimensions`**——否则 `CSI 6;h;w t` 回复会被消费却
    /// 永远不更新全局单元格尺寸。宿主自行赋值可覆盖。
    pub cell_size_handler: Option<Box<dyn FnMut(usize, usize)>>,
    /// `getCapabilities().images` 的默认来源（TS `queryCellSize` 的 `if (!getCapabilities().images) return`）。
    ///
    /// 默认指向 `terminal_image::get_capabilities`，因此两个 TUI 类无需各自注入；宿主赋值可覆盖。
    pub images_capability: Option<Box<dyn Fn() -> bool>>,
    /// 终端输入队列（`Terminal::start` 的回调把数据推进来）。
    pending_input: Rc<RefCell<Vec<String>>>,
    /// overlay 栈变化时调用的渲染请求回调（实际渲染由具体 TUI 的循环负责）。
    redraw_requested: usize,
    /// 被移出挂载树但仍持有焦点的子树（TS `focusedComponent` 的引用语义）。
    detached_focus: Option<DetachedFocus>,
}

impl TuiBase {
    /// 构造（TS `constructor(terminal, showHardwareCursor?, logDirectory?)`）。
    pub fn new(
        terminal: Box<dyn Terminal>,
        show_hardware_cursor: Option<bool>,
        log_directory: Option<String>,
    ) -> Self {
        let terminal = Rc::new(RefCell::new(terminal));
        // `isOverlayVisible` 每次都读**实时**终端尺寸（TS `this.terminal.columns/rows`）。
        let terminal_size: Rc<dyn Fn() -> (usize, usize)> = {
            let terminal = Rc::clone(&terminal);
            Rc::new(move || {
                let terminal = terminal.borrow();
                (terminal.columns(), terminal.rows())
            })
        };
        let render_requested = Rc::new(std::cell::Cell::new(false));
        let hide_terminal = Rc::clone(&terminal);
        let hide_cursor: Rc<dyn Fn()> = Rc::new(move || {
            hide_terminal.borrow_mut().hide_cursor();
        });
        let stopped = Rc::new(std::cell::Cell::new(false));
        let mount = Rc::new(RefCell::new(MountState::default()));
        let core = Rc::new(RefCell::new(TuiCore::new(
            terminal_size,
            Rc::clone(&render_requested),
            hide_cursor,
            Rc::clone(&stopped),
            mount,
        )));
        TuiBase {
            terminal,
            children: Vec::new(),
            layout_root: None,
            hooks: None,
            exit_restore_registered: false,
            core,
            on_debug: None,
            show_hardware_cursor: show_hardware_cursor.unwrap_or(false),
            clear_on_shrink: false,
            full_redraw_count: 0,
            stopped,
            log_directory,
            terminal_color_scheme_notifications_enabled: false,
            input_listeners: Vec::new(),
            next_listener_id: 0,
            terminal_color_scheme_listeners: Vec::new(),
            next_scheme_listener_id: 0,
            pending_terminal_color_queries: Rc::new(RefCell::new(Vec::new())),
            next_color_query_id: 0,
            render_requested,
            immediate_render_scheduled: false,
            last_render_at: 0.0,
            timer: Box::new(NoopTimer),
            clock: Box::new(monotonic_clock),
            // TS `consumeCellSizeResponse` 直接调 `setCellDimensions({ widthPx, heightPx })`；
            // 默认就指向 `terminal-image` 的实现（常量在 `terminal_image` 里）。
            cell_size_handler: Some(Box::new(|width_px, height_px| {
                set_cell_dimensions(CellDimensions {
                    width_px: width_px as f64,
                    height_px: height_px as f64,
                });
            })),
            // TS `queryCellSize` 直接读 `getCapabilities().images`。
            images_capability: Some(Box::new(|| get_capabilities().images.is_some())),
            pending_input: Rc::new(RefCell::new(Vec::new())),
            redraw_requested: 0,
            detached_focus: None,
        }
    }

    /// 注入时钟（`performance.now()` 的等价物）。
    pub fn set_clock(&mut self, clock: Box<dyn Fn() -> f64>) {
        self.clock = clock;
    }

    /// 注入定时器。
    pub fn set_timer(&mut self, timer: Box<dyn Timer>) {
        self.timer = timer;
    }

    /// 共享核心状态（测试与具体渲染器使用）。
    pub fn core(&self) -> &Rc<RefCell<TuiCore>> {
        &self.core
    }

    /// 渲染请求标志的共享句柄。
    ///
    /// TS 的 `ScrollView` 在布局时保存 `() => this.requestRender()`、`AltScreenFlashContainer`
    /// 在构造时保存同一回调；Rust 的组件回调是 `'static` 闭包，不能借用宿主，因此把
    /// `TuiBase.renderRequested` 的 `Rc<Cell<bool>>` 交给它们，让回调与
    /// `TuiBase::request_render(false)` 一样置位宿主的调度标志。
    pub fn render_requested_handle(&self) -> Rc<std::cell::Cell<bool>> {
        Rc::clone(&self.render_requested)
    }

    /// `getFocusedComponent`。
    ///
    /// TS 返回组件引用；Rust 的 [`FocusTarget`] 只能表示挂载树里的位置。焦点指向一个已被
    /// 移出挂载树的组件（`DetachedFocus`）时返回 `None`——调用方无法把这个"位置"交给
    /// 另一个 TUI。
    pub fn focused_component(&self) -> Option<FocusTarget> {
        if self.detached_focus_active() {
            return None;
        }
        self.core.borrow().focused()
    }

    /// `hasOverlayEntries`（含隐藏项）。
    pub fn has_overlay_entries(&self) -> bool {
        self.core.borrow().has_overlay_entries()
    }

    /// `fullRedraws`。
    pub fn full_redraws(&self) -> u64 {
        self.full_redraw_count
    }

    /// `getShowHardwareCursor`。
    pub fn get_show_hardware_cursor(&self) -> bool {
        self.show_hardware_cursor
    }

    /// `getClearOnShrink`。
    pub fn get_clear_on_shrink(&self) -> bool {
        self.clear_on_shrink
    }

    /// 兼容入口：尺寸由 [`TuiBase::terminal`] 实时读取，这里只记为一次渲染尺寸同步。
    ///
    /// TS 的 `isOverlayVisible` 每次都读 `terminal.columns/rows`（`tui.ts:925-931`），没有缓存；
    /// Rust 侧同样直接读终端，因此本方法不再影响 `visible()` 判定，保留只是为了源码兼容。
    pub fn set_terminal_size(&mut self, _columns: usize, _rows: usize) {}

    /// `getMountedRoots()`：`layoutRoot` 存在时**只**返回它，否则返回 `children`
    /// （`tui-alt-screen.ts:339`）。返回地址而非引用，供挂载判定使用。
    fn mounted_root_addresses(&self) -> Vec<ComponentKey> {
        let mut roots = Vec::new();
        if let Some(root) = self.layout_root.as_ref() {
            collect_component_addresses(root.as_ref(), &mut roots);
            return roots;
        }
        for child in &self.children {
            collect_component_addresses(child.as_ref(), &mut roots);
        }
        roots
    }

    /// 按 [`FocusTarget`] 找到组件身份（`None` = 目标不存在）。
    fn component_key_of(&self, target: FocusTarget) -> Option<ComponentKey> {
        match target {
            FocusTarget::Component { index } => self
                .children
                .get(index)
                .map(|child| component_key(child.as_ref())),
            FocusTarget::Path { path } => {
                let indices = path.indices();
                let root: &dyn Component = self.children.get(*indices.first()? as usize)?.as_ref();
                component_key_at_path(root, &indices[1..])
            }
            FocusTarget::LayoutRoot { path } => {
                let root: &dyn Component = self.layout_root.as_deref()?;
                component_key_at_path(root, path.indices())
            }
            FocusTarget::Overlay { .. } => None,
        }
    }

    /// 组件地址 → [`FocusTarget`]（[`MouseFocusTarget::Address`] 的解析）。
    ///
    /// 先找布局根（`setLayoutRoot` 之后 TS 的挂载根就是它），再找 `children`。找不到时返回
    /// `None`（组件已经脱离树），由调用方退回"本次分发的接收者"。
    pub fn focus_target_of_address(&self, address: usize) -> Option<FocusTarget> {
        if let Some(root) = self.layout_root.as_ref()
            && let Some(path) = component_path_to_address(root.as_ref(), address)
        {
            return Some(FocusTarget::LayoutRoot {
                path: focus_path_from(path),
            });
        }
        for (index, child) in self.children.iter().enumerate() {
            let Some(path) = component_path_to_address(child.as_ref(), address) else {
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

    /// 刷新 `isComponentMounted` 的挂载快照（每次可能改变树或焦点前调用）。
    fn refresh_mount_state(&self) {
        let addresses = self.mounted_root_addresses();
        self.core.borrow_mut().mount.borrow_mut().addresses = addresses;
    }

    /// 记录"某个焦点目标此刻指向哪个组件地址"，供后续的挂载判定复现 TS 的引用相等语义。
    fn remember_focus_address(&mut self, target: Option<FocusTarget>) {
        let Some(target) = target else {
            return;
        };
        if matches!(target, FocusTarget::Overlay { .. }) {
            return;
        }
        let Some(address) = self.component_key_of(target) else {
            return;
        };
        self.core
            .borrow_mut()
            .mount
            .borrow_mut()
            .remember(target, address);
    }

    /// `addChild`。
    pub fn add_child(&mut self, component: Box<dyn Component>) {
        self.children.push(component);
        self.refresh_mount_state();
    }

    /// 结构变化前记录"所有仍指向挂载组件的焦点目标 → 组件地址"。
    ///
    /// 覆盖 `focused`、overlay 恢复状态里的 `blockedBy` / `resume.target` 以及每个 overlay 的
    /// `preFocus`（TS 里它们都是组件引用，删除子项后仍指向同一个对象）。
    fn snapshot_focus_targets(&self) -> Vec<(FocusTarget, ComponentKey)> {
        let mut targets: Vec<FocusTarget> = Vec::new();
        {
            let core = self.core.borrow();
            if let Some(target) = core.focused() {
                targets.push(target);
            }
            if let OverlayFocusRestoreState::Blocked {
                blocked_by, resume, ..
            } = core.overlay_focus_restore_state()
            {
                if let Some(target) = blocked_by {
                    targets.push(target);
                }
                if let OverlayBlockedFocusResume::FocusTarget {
                    target: Some(target),
                } = resume
                {
                    targets.push(target);
                }
            }
            for entry in core.overlay_stack() {
                if let Some(target) = entry.pre_focus {
                    targets.push(target);
                }
            }
        }
        let mut snapshot: Vec<(FocusTarget, ComponentKey)> = Vec::new();
        for target in targets {
            if matches!(target, FocusTarget::Overlay { .. })
                || snapshot.iter().any(|(known, _)| *known == target)
            {
                continue;
            }
            if let Some(address) = self.component_key_of(target) {
                snapshot.push((target, address));
            }
        }
        snapshot
    }

    /// 按快照把焦点目标重映射到同一组件的新下标（删除/清空子项后调用）。
    fn remap_focus_targets(&mut self, snapshot: &[(FocusTarget, ComponentKey)]) {
        let mut mapping: Vec<(FocusTarget, FocusTarget, ComponentKey)> = Vec::new();
        for &(old, address) in snapshot {
            let Some(new) = self.focus_target_of_address(address.0) else {
                continue;
            };
            if new != old {
                mapping.push((old, new, address));
            }
        }
        if mapping.is_empty() {
            return;
        }
        let pairs: Vec<(FocusTarget, FocusTarget)> =
            mapping.iter().map(|(old, new, _)| (*old, *new)).collect();
        self.core.borrow_mut().apply_focus_target_mapping(&pairs);
        let core = self.core.borrow_mut();
        let mut mount = core.mount.borrow_mut();
        for &(_, new, address) in &mapping {
            mount.remember(new, address);
        }
    }

    /// 脱离焦点是否仍然有效（创建之后焦点没有被重新设置过）。
    fn detached_focus_active(&self) -> bool {
        match self.detached_focus.as_ref() {
            Some(detached) => detached.epoch == self.core.borrow().focus_epoch(),
            None => false,
        }
    }

    /// 释放脱离焦点：TS `setFocusInternal` 会把旧 `focusedComponent.focused` 置 false。
    fn release_detached_focus(&mut self) {
        if let Some(mut detached) = self.detached_focus.take() {
            clear_focus_flags(detached.component.as_mut());
        }
    }

    /// 删除子项后维护焦点（TS 的 `focusedComponent` 是组件引用，不受 `children` 变化影响）。
    ///
    /// - 被删子树包含焦点 → 保留为 [`DetachedFocus`]（后续按键仍投递给它），返回 `None`；
    /// - 其余情况按 `snapshot` 把焦点目标重映射到同一组件的新下标（见
    ///   [`TuiBase::remap_focus_targets`]）；
    /// - 原样返回被删子树。
    fn reconcile_focus_after_removal(
        &mut self,
        removed: Box<dyn Component>,
        snapshot: &[(FocusTarget, ComponentKey)],
    ) -> Option<Box<dyn Component>> {
        let focused = self.core.borrow().focused();
        let focused_address = focused.and_then(|target| {
            snapshot
                .iter()
                .find(|(known, _)| *known == target)
                .map(|(_, address)| *address)
        });
        if self.detached_focus.is_none()
            && let Some(address) = focused_address
            && let Some(path) = component_path_to_address(removed.as_ref(), address.0)
        {
            let epoch = self.core.borrow().focus_epoch();
            self.detached_focus = Some(DetachedFocus {
                component: removed,
                path: path.into_iter().map(|index| index as u32).collect(),
                epoch,
            });
            self.remap_focus_targets(snapshot);
            return None;
        }
        self.remap_focus_targets(snapshot);
        Some(removed)
    }

    /// `removeChild`：按指针相等删除；未找到时 no-op。焦点处理与
    /// [`TuiBase::remove_child_at`] 相同。
    pub fn remove_child(&mut self, component: &dyn Component) {
        let target = component as *const dyn Component as *const ();
        if let Some(index) = self
            .children
            .iter()
            .position(|child| child.as_ref() as *const dyn Component as *const () == target)
        {
            self.remove_child_at(index);
        }
    }

    /// `clear()`：清空全部顶层子项。
    ///
    /// 先对焦点目标做快照，再逐个 `reconcile_focus_after_removal`：仍挂载的目标按下标重映射；
    /// 持有焦点的子树留在 `DetachedFocus` 里继续接收键盘输入（复现 TS `focusedComponent`
    /// 的引用语义）。
    pub fn clear_children(&mut self) {
        let snapshot = self.snapshot_focus_targets();
        let children = std::mem::take(&mut self.children);
        self.refresh_mount_state();
        for child in children {
            let _ = self.reconcile_focus_after_removal(child, &snapshot);
        }
    }

    /// 按槽位移除子项并返回它（调用方决定保活或复用）。
    ///
    /// TS 的 `children` 是 protected 数组，子类/调用方可以直接 `splice` 并保留组件引用；
    /// Rust 需要这个入口才能在"移出树"之后继续持有组件（例如稍后原样放回）。
    /// 若被删子树持有焦点，它会被留在 `DetachedFocus` 里并返回 `None`。
    pub fn remove_child_at(&mut self, index: usize) -> Option<Box<dyn Component>> {
        if index >= self.children.len() {
            return None;
        }
        let snapshot = self.snapshot_focus_targets();
        let child = self.children.remove(index);
        self.refresh_mount_state();
        self.reconcile_focus_after_removal(child, &snapshot)
    }

    /// `setLayoutRoot(component)`（`TuiAltScreen`）：设置后它**取代** `children` 成为挂载根
    /// （`getMountedRoots()` 的 `layoutRoot ? [layoutRoot] : children`）。
    pub fn set_layout_root(&mut self, component: Option<Box<dyn Component>>) {
        self.layout_root = component;
        self.refresh_mount_state();
    }

    /// 安装生命周期钩子（TS 的 `beforeTerminalStart` / `afterTerminalStart` /
    /// `beforeTerminalStop` / `afterTerminalStop` / `getExitRestorePrefix` /
    /// `getExitRestoreSuffix` 覆写点；具体渲染器在构造后调用一次）。
    pub fn set_lifecycle_hooks(&mut self, hooks: Box<dyn TuiLifecycleHooks>) {
        self.hooks = Some(Rc::new(RefCell::new(hooks)));
    }

    fn hooks(&self) -> Option<Rc<RefCell<Box<dyn TuiLifecycleHooks>>>> {
        self.hooks.as_ref().map(Rc::clone)
    }

    /// 把 `focused` 标志同步到挂载的组件树（`children` 或 `layoutRoot`）与 overlay 组件。
    ///
    /// overlay handle 的每条路径都会在改完 `TuiCore` 之后调用本方法（handle 持共享状态，
    /// 因此无法直接借用 `TuiBase`；见 [`TuiCore::take_focus_dirty`]）。
    ///
    /// TS 的 `setFocusInternal` 只把旧的聚焦组件置 false、新的置 true；这里先递归清零整棵
    /// 挂载树再给目标置 true，可观察结果相同（只有 TUI 会写这个标志），并且能正确清除嵌套
    /// 目标残留的 `focused`。
    pub fn sync_focus_flags(&mut self) {
        let focused = self.core.borrow().focused();
        let focus_epoch = self.core.borrow().focus_epoch();
        for child in &mut self.children {
            clear_focus_flags(child.as_mut());
        }
        if let Some(root) = self.layout_root.as_mut() {
            clear_focus_flags(root.as_mut());
        }
        // TS 的 `focusedComponent` 是引用：被 `removeChild`/`clear` 移出树的组件仍然是焦点，
        // 标志只写在它上面；此时挂载树里的同下标组件**不是**焦点（不能给它置位）。
        match self.detached_focus.take() {
            Some(mut detached) if detached.epoch == focus_epoch => {
                set_focus_flag_at(detached.component.as_mut(), &detached.path);
                self.detached_focus = Some(detached);
            }
            Some(mut detached) => {
                // 焦点已被重新设置：TS `setFocusInternal` 把旧组件的标志置 false。
                clear_focus_flags(detached.component.as_mut());
            }
            None => match focused {
                Some(FocusTarget::Component { index }) => {
                    if let Some(child) = self.children.get_mut(index) {
                        set_focus_flag_at(child.as_mut(), &[]);
                    }
                }
                Some(FocusTarget::Path { path }) => {
                    let indices = path.indices();
                    if let Some((first, rest)) = indices.split_first()
                        && let Some(child) = self.children.get_mut(*first as usize)
                    {
                        set_focus_flag_at(child.as_mut(), rest);
                    }
                }
                Some(FocusTarget::LayoutRoot { path }) => {
                    if let Some(root) = self.layout_root.as_mut() {
                        set_focus_flag_at(root.as_mut(), path.indices());
                    }
                }
                Some(FocusTarget::Overlay { .. }) | None => {}
            },
        }
        let mut core = self.core.borrow_mut();
        core.apply_focus_flags_to_overlays();
    }

    /// handle 改过焦点后调用：把 `focused` 标志刷到真实组件上（TS 的 handle 直接改组件属性）。
    pub fn flush_pending_focus_sync(&mut self) {
        if !self.core.borrow_mut().take_focus_dirty() {
            return;
        }
        self.refresh_mount_state();
        self.sync_focus_flags();
    }

    /// `setFocus(component)`。
    pub fn set_focus(&mut self, target: Option<FocusTarget>) {
        // TS `setFocusInternal` 先把旧的 `focusedComponent.focused` 置 false；旧焦点可能是
        // 已脱离挂载树的组件（见 `DetachedFocus`）。
        self.release_detached_focus();
        self.refresh_mount_state();
        // 记录新目标的地址必须在 `set_focus_internal` **之后**：TS 在内部先判定旧的
        // `blockedBy` 是否仍挂载（`isComponentMounted`），此时旧目标的地址不能被新组件覆盖
        // （两者可能是同一个下标，例如"清空后在下标 0 重新挂一个组件再 setFocus"）。
        self.core.borrow_mut().set_focus(target);
        self.remember_focus_address(target);
        self.sync_focus_flags();
    }

    /// `showOverlay(component, options?)`。Rust 的 `options` 必填；没有选项时传
    /// `OverlayOptions::default()`（与 TS 的 `undefined` 语义相同）。
    pub fn show_overlay(
        &mut self,
        component: Box<dyn Component>,
        options: OverlayOptions,
    ) -> Box<dyn OverlayHandle> {
        self.refresh_mount_state();
        let non_capturing = options.non_capturing == Some(true);
        let (id, visible) = {
            let mut core = self.core.borrow_mut();
            let id = core.push_overlay(component, Some(options));
            let index = core.overlay_index(id).expect("刚插入的 overlay 在栈中");
            (id, core.is_overlay_visible(&core.overlay_stack[index]))
        };
        if !non_capturing && visible {
            self.core
                .borrow_mut()
                .set_focus(Some(FocusTarget::Overlay { id }));
        }
        // `set_focus_internal` 置了脏位；handle 路径同样依赖这个统一刷新点。
        self.flush_pending_focus_sync();
        self.core.borrow().hide_terminal_cursor();
        self.request_render(false);
        Box::new(TuiOverlayHandle {
            core: Rc::clone(&self.core),
            id,
        })
    }

    /// `hideOverlay()`：隐藏栈顶并恢复焦点。
    pub fn hide_overlay(&mut self) {
        {
            let mut core = self.core.borrow_mut();
            let Some(entry_id) = core.overlay_stack.last().map(|entry| entry.id) else {
                return;
            };
            core.clear_overlay_focus_restore_for(entry_id);
            core.retarget_overlay_pre_focus(entry_id);
            let Some(entry) = core.remove_overlay(entry_id) else {
                return;
            };
            if core.focused == Some(FocusTarget::Overlay { id: entry_id }) {
                let next = core
                    .topmost_visible_overlay()
                    .map(|overlay| FocusTarget::Overlay { id: overlay })
                    .or(entry.pre_focus);
                core.set_focus(next);
            }
            if core.overlay_stack.is_empty() {
                core.hide_terminal_cursor();
            }
        }
        // `set_focus` 可能因 restore 状态改写了实际焦点，统一按脏位刷新。
        self.flush_pending_focus_sync();
        self.request_render(false);
    }

    /// `hasOverlay()`：存在可见 overlay。
    pub fn has_overlay(&self) -> bool {
        self.core.borrow().has_visible_overlay()
    }

    /// `isOverlayFocused()`。
    pub fn is_overlay_focused(&self) -> bool {
        self.core.borrow().is_overlay_focused()
    }

    /// `resolveMouseFocusTarget(component)`（`tui.ts:922-930`）：命中组件在某个可见 overlay
    /// 内部时，焦点交给那个 overlay 本身（从栈顶往下找第一个包含它的可见 overlay）。
    pub fn resolve_mouse_focus_target(&self, target: FocusTarget) -> FocusTarget {
        if matches!(target, FocusTarget::Overlay { .. }) {
            return self.core.borrow().resolve_mouse_focus_target(target);
        }
        let Some(address) = self.component_key_of(target) else {
            return target;
        };
        let core = self.core.borrow();
        for entry in core.overlay_stack.iter().rev() {
            if !core.is_overlay_visible(entry) {
                continue;
            }
            let mut addresses = Vec::new();
            collect_component_addresses(entry.component.as_ref(), &mut addresses);
            if addresses.contains(&address) {
                return FocusTarget::Overlay { id: entry.id };
            }
        }
        target
    }

    /// `applyMouseDispatchResult(event, result)` 的焦点部分（`spec/tui-core.md` §2.13.8）：
    ///
    /// ```text
    /// focusTarget = resolveMouseFocusTarget(result.focusTarget ?? result.target.component)
    /// focusChanged = result.focus === true && getFocusedComponent() !== focusTarget
    /// if (result.focus) setFocus(focusTarget)
    /// ```
    ///
    /// 返回 `focusChanged`；`capture` 与"是否渲染"由调用方（备用屏渲染器）处理。
    /// `fallback_target` 是"命中的组件本身"（`result.target.component` 的 Rust 等价物）：
    /// 结果没带地址（例如调用方直接构造）时才用它。带 [`MouseFocusTarget::Address`] 时按地址
    /// 在挂载树里解析——TS 的 `result.target.component` 是**命中的最内层组件**，不是分发起点。
    pub fn apply_mouse_focus_result(
        &mut self,
        result: &TuiMouseEventResult,
        fallback_target: FocusTarget,
    ) -> bool {
        if result.focus != Some(true) {
            return false;
        }
        let dispatch_target = match result.focus_target {
            Some(MouseFocusTarget::Explicit(target)) => target,
            Some(MouseFocusTarget::Address(address)) => self
                .focus_target_of_address(address)
                .unwrap_or(fallback_target),
            Some(MouseFocusTarget::SelfComponent) | None => fallback_target,
        };
        let focus_target = self.resolve_mouse_focus_target(dispatch_target);
        let changed = self.focused_component() != Some(focus_target);
        self.set_focus(Some(focus_target));
        changed
    }

    /// `hideTerminalCursor()`：未 `stop()` 时隐藏终端光标（判定在 [`TuiCore`] 里）。
    pub fn hide_terminal_cursor(&self) {
        self.core.borrow().hide_terminal_cursor();
    }

    /// `invalidate()`：所有 `getMountedRoots()` 与 overlay 组件（`tui.ts:913-917`）。
    pub fn invalidate_all(&mut self) {
        if let Some(root) = self.layout_root.as_mut() {
            root.invalidate();
        } else {
            for root in &mut self.children {
                root.invalidate();
            }
        }
        let mut core = self.core.borrow_mut();
        for entry in &mut core.overlay_stack {
            entry.component.invalidate();
        }
    }

    /// `TuiBase.getExitRestorePrefix()`：`terminalColorSchemeNotificationsEnabled ? "\x1b[?2031l" : ""`
    /// + `"\x1b[?25h"`（TS 里子类覆写时会先调 `super.getExitRestorePrefix()`）。
    fn base_exit_restore_prefix(&self) -> String {
        format!(
            "{}\x1b[?25h",
            if self.terminal_color_scheme_notifications_enabled {
                "\x1b[?2031l"
            } else {
                ""
            }
        )
    }

    /// `getExitRestorePrefix()`：`TuiBase` 默认值 + 子类钩子（`tui-alt-screen.ts:431-436`）。
    pub fn exit_restore_prefix(&mut self) -> String {
        let base = self.base_exit_restore_prefix();
        match self.hooks() {
            Some(hooks) => hooks.borrow_mut().exit_restore_prefix(&base),
            None => base,
        }
    }

    /// `getExitRestoreSuffix()`：`TuiBase` 默认 `""` + 子类钩子（`tui-alt-screen.ts:438-441`）。
    pub fn exit_restore_suffix(&mut self) -> String {
        match self.hooks() {
            Some(hooks) => hooks.borrow_mut().exit_restore_suffix(""),
            None => String::new(),
        }
    }

    /// `exitRestoreHandler`（`tui.ts:578-589`）：进程退出路径的兜底恢复。
    ///
    /// TS 用 `process.on("exit", …)`；Rust 没有全局 exit 事件，由宿主（例如
    /// `kiana-coding-agent` 的启动流程）或测试在进程收尾时调用本方法。行为：
    /// `stopped` 时什么都不做。
    pub fn run_exit_restore_handler(&mut self) {
        if self.stopped.get() {
            return;
        }
        let before = self.exit_restore_prefix();
        let after = self.exit_restore_suffix();
        self.terminal.borrow_mut().restore_on_exit(&before, &after);
    }

    /// 退出处理器当前是否已注册（`process.on("exit", …)` 的挂载状态）。
    pub fn exit_restore_registered(&self) -> bool {
        self.exit_restore_registered
    }

    /// `queryCellSize()`：仅在终端支持图片时发送 `CSI 16 t`（`tui.ts:996-1004`）。
    ///
    /// TS 判 `getCapabilities().images`；默认实现（[`TuiBase::new`]）就是
    /// `terminal_image::get_capabilities()`，与 TS 相同。宿主可用
    /// [`TuiBase::images_capability`] 覆盖（`None` 等价于 `images: null`，早退不发查询）。
    pub fn query_cell_size(&mut self) {
        let supported = self
            .images_capability
            .as_ref()
            .is_some_and(|capability| capability());
        if !supported {
            return;
        }
        self.terminal.borrow_mut().write("\x1b[16t");
    }

    /// `start()`（`tui.ts:950-966`）：
    /// `stopped=false` → 挂 exit 处理器 → `beforeTerminalStart()` → `terminal.start` →
    /// `afterTerminalStart()` → `hideCursor()` → `?2031h` → `queryCellSize()` → `requestRender()`。
    ///
    /// Rust 没有全局 exit 事件，"挂 exit 处理器"在这里只表示把 `exit_restore_registered`
    /// 置为 true；真正的退出恢复由宿主调用 [`TuiBase::run_exit_restore_handler`] 完成。
    pub fn start(&mut self) {
        self.stopped.set(false);
        // TS：`process.off("exit", h); process.on("exit", h)` —— 重复 start 不会重复挂载。
        self.exit_restore_registered = true;
        self.refresh_mount_state();
        if let Some(hooks) = self.hooks() {
            hooks.borrow_mut().before_terminal_start();
        }
        let pending_input = Rc::clone(&self.pending_input);
        let render_requested = Rc::clone(&self.render_requested);
        self.terminal.borrow_mut().start(
            Box::new(move |data| pending_input.borrow_mut().push(data.to_string())),
            Box::new(move || render_requested.set(true)),
        );
        if let Some(hooks) = self.hooks() {
            hooks.borrow_mut().after_terminal_start();
        }
        self.terminal.borrow_mut().hide_cursor();
        if self.terminal_color_scheme_notifications_enabled {
            self.terminal.borrow_mut().write("\x1b[?2031h");
        }
        self.query_cell_size();
        self.request_render(false);
    }

    /// `stop(options = {})`（`tui.ts:1006-1017`）：
    /// `stopped=true` → 摘 exit 处理器 → `cancelRenderTimer()` → `?2031l` →
    /// `beforeTerminalStop(options)` → `showCursor()` → `terminal.stop()` → `afterTerminalStop(options)`。
    pub fn stop(&mut self, options: TuiStopOptions) {
        self.stopped.set(true);
        self.exit_restore_registered = false;
        self.cancel_render_timer();
        if self.terminal_color_scheme_notifications_enabled {
            self.terminal.borrow_mut().write("\x1b[?2031l");
        }
        if let Some(hooks) = self.hooks() {
            hooks.borrow_mut().before_terminal_stop(options);
        }
        self.terminal.borrow_mut().show_cursor();
        self.terminal.borrow_mut().stop();
        if let Some(hooks) = self.hooks() {
            hooks.borrow_mut().after_terminal_stop(options);
        }
    }

    /// 取出 `Terminal::start` 回调里排队的输入（宿主循环每轮调用），**不**分发。
    pub fn drain_pending_input(&mut self) -> Vec<String> {
        std::mem::take(&mut *self.pending_input.borrow_mut())
    }

    /// 取出并逐条分发给 [`TuiBase::handle_terminal_input`]
    /// （`Terminal::start` 的 `onInput` → `handleTerminalInput` 等价物，宿主循环每轮调用）。
    ///
    /// 分发的每条输入都会在结束后刷新 overlay handle 可能挂起的 `focused` 标志。
    pub fn pump_pending_input(&mut self) {
        loop {
            let next = {
                let mut queue = self.pending_input.borrow_mut();
                if queue.is_empty() {
                    None
                } else {
                    Some(queue.remove(0))
                }
            };
            let Some(data) = next else { break };
            self.handle_terminal_input(&data);
            self.flush_pending_focus_sync();
        }
    }

    /// `setShowHardwareCursor(enabled)`。
    pub fn set_show_hardware_cursor(&mut self, enabled: bool) {
        if self.show_hardware_cursor == enabled {
            return;
        }
        self.show_hardware_cursor = enabled;
        if !enabled {
            self.hide_terminal_cursor();
        }
        self.request_render(false);
    }

    /// `setClearOnShrink(enabled)`（**不**触发渲染）。
    pub fn set_clear_on_shrink(&mut self, enabled: bool) {
        self.clear_on_shrink = enabled;
    }

    /// `setTerminalColorSchemeNotifications(enabled)`。
    pub fn set_terminal_color_scheme_notifications(&mut self, enabled: bool) {
        if self.terminal_color_scheme_notifications_enabled == enabled {
            return;
        }
        self.terminal_color_scheme_notifications_enabled = enabled;
        if !self.stopped.get() {
            self.terminal.borrow_mut().write(if enabled {
                "\x1b[?2031h"
            } else {
                "\x1b[?2031l"
            });
        }
    }

    /// `onTerminalColorSchemeChange(listener)`：返回监听器 id（用
    /// [`TuiBase::remove_terminal_color_scheme_listener`] 取消）。
    pub fn on_terminal_color_scheme_change(
        &mut self,
        listener: Box<dyn FnMut(TerminalColorScheme)>,
    ) -> usize {
        self.next_scheme_listener_id += 1;
        let id = self.next_scheme_listener_id;
        self.terminal_color_scheme_listeners.push((id, listener));
        id
    }

    /// 移除明暗方案监听器。
    pub fn remove_terminal_color_scheme_listener(&mut self, id: usize) {
        self.terminal_color_scheme_listeners
            .retain(|(listener_id, _)| *listener_id != id);
    }

    /// `addInputListener(listener)`。
    pub fn add_input_listener(&mut self, listener: TuiInputListener) -> InputListenerId {
        self.next_listener_id += 1;
        let id = InputListenerId(self.next_listener_id);
        self.input_listeners.push((id, listener));
        id
    }

    /// `removeInputListener(listener)`。
    pub fn remove_input_listener(&mut self, id: InputListenerId) {
        self.input_listeners
            .retain(|(listener_id, _)| *listener_id != id);
    }

    /// 兼容别名：[`TuiBase::query_cell_size`]（早期骨架里把"是否支持图片"的判断留给调用方）。
    pub fn request_cell_size(&mut self) {
        self.query_cell_size();
    }

    /// `renderNow(force = false)` 的前半段：重置状态并记录渲染时刻。
    pub fn begin_render_now(&mut self, force: bool) {
        if force {
            self.reset_render_state();
        }
        self.render_requested.set(false);
        self.cancel_render_timer();
        self.last_render_at = (self.clock)();
    }

    /// `resetRenderState()`（默认空；具体渲染器覆盖）。
    pub fn reset_render_state(&mut self) {}

    /// `requestRender(force = false)`。
    pub fn request_render(&mut self, force: bool) {
        if force {
            self.reset_render_state();
            self.request_immediate_render();
            return;
        }
        if self.render_requested.get() {
            return;
        }
        self.render_requested.set(true);
    }

    /// overlay 栈发生变化（handle 内部触发）后，外部是否需要重绘。
    pub fn take_redraw_request(&mut self) -> bool {
        let requested = self.render_requested.get();
        if requested {
            self.redraw_requested = self.redraw_requested.wrapping_add(1);
        }
        requested
    }

    fn request_immediate_render(&mut self) {
        self.cancel_render_timer();
        self.render_requested.set(true);
        self.immediate_render_scheduled = true;
    }

    fn cancel_render_timer(&mut self) {
        self.timer.cancel();
    }

    /// 宿主渲染循环：是否需要（以及何时）渲染。
    ///
    /// TS 的 `scheduleRender`（`tui.ts:1053-1075`）用 `performance.now()`：`lastRenderAt`
    /// 初值 0，所以**第一次**调度必然 `delay = max(0, 16 - now) = 0`，下一拍就渲染；此后每次
    /// 渲染都会把 `lastRenderAt` 推到当前时刻，节流窗口才真正生效。默认时钟
    /// （[`monotonic_clock`]）与之一致：返回值随真实时间前进，因此本方法不会永远返回 `Wait`。
    pub fn render_decision(&mut self) -> RenderDecision {
        if self.stopped.get() || !self.render_requested.get() {
            return RenderDecision::Idle;
        }
        if self.immediate_render_scheduled {
            self.immediate_render_scheduled = false;
            return RenderDecision::Now;
        }
        let elapsed = (self.clock)() - self.last_render_at;
        let delay = (MIN_RENDER_INTERVAL_MS - elapsed).max(0.0);
        if delay > 0.0 {
            RenderDecision::Wait(delay)
        } else {
            RenderDecision::Now
        }
    }

    /// 宿主决定渲染后调用：消费渲染请求并记录渲染时刻（TS `renderNow` 的头部，
    /// `tui.ts:1024-1030`），返回是否真的需要渲染。
    pub fn take_render_decision_now(&mut self) -> bool {
        if !matches!(self.render_decision(), RenderDecision::Now) {
            return false;
        }
        self.render_requested.set(false);
        self.last_render_at = (self.clock)();
        true
    }

    /// `handleTerminalInput(data)`——处理顺序固定（见 `spec/tui-core.md` §2.9.11）。
    pub fn handle_terminal_input(&mut self, data: &str) {
        // 0. overlay handle（`hide`/`setHidden`/`unfocus`…）改焦点时只写共享状态；TS 的 handle
        //    直接改组件属性，因此这里先把挂起的 `focused` 标志刷到真实组件（见
        //    [`TuiBase::flush_pending_focus_sync`]），保证本函数看到与 TS 相同的组件状态。
        self.flush_pending_focus_sync();

        // 1-2. 终端配色回复 / 明暗方案上报。
        if self.consume_terminal_color_response(data) {
            return;
        }
        if self.consume_terminal_color_scheme_report(data) {
            return;
        }

        // 3. 输入监听器（Set 插入顺序；consume 终止，data 替换当前输入）。
        let mut current = data.to_string();
        if !self.input_listeners.is_empty() {
            for (_, listener) in &mut self.input_listeners {
                let Some(result) = listener(&current) else {
                    continue;
                };
                if result.consume == Some(true) {
                    return;
                }
                if let Some(next) = result.data {
                    current = next;
                }
            }
            if current.is_empty() {
                return;
            }
        }

        // 4. 终端 cell 尺寸回复。
        if self.consume_cell_size_response(&current) {
            return;
        }

        // 5. 全局调试键（Shift+Ctrl+D）。
        if crate::keys::matches_key(&current, "shift+ctrl+d")
            && let Some(on_debug) = self.on_debug.as_mut()
        {
            on_debug();
            return;
        }

        // 6. 聚焦的 overlay 若已不可见（尺寸变化或 visible() 回调），改投最上层可见的
        //    捕获型 overlay（topmost_visible_overlay 跳过 non_capturing），没有则回到
        //    它的 preFocus（preserve：不清除恢复状态）。
        self.repair_invisible_focused_overlay();

        // 7. 焦点不在 overlay 上时，按**第 6 步之后**的恢复状态重新判定（TS 在这里重新调用
        //    `getVisibleOverlayFocusRestore()`，因此第 6 步刚改过的状态会被看到）。
        self.restore_overlay_focus_before_input();

        // 8. 交给聚焦组件（含 Ctrl+C；组件自己决定怎么处理）。
        //
        // TS 的 `focusedComponent` 是引用：组件被 `removeChild`/`clear` 移出挂载树后仍然
        // 接收输入。Rust 把这种子树保留在 `detached_focus` 里，继续按引用语义投递。
        if self.detached_focus_active() {
            let mut handled = false;
            {
                let detached = self
                    .detached_focus
                    .as_mut()
                    .expect("detached_focus_active 保证存在");
                with_component_mut_at_path(
                    detached.component.as_mut(),
                    &detached.path,
                    &mut |component| {
                        if !component.handles_input() {
                            return;
                        }
                        if crate::keys::is_key_release(&current) && !component.wants_key_release() {
                            return;
                        }
                        component.handle_input(&current);
                        handled = true;
                    },
                );
            }
            if handled {
                self.request_immediate_render();
            }
            return;
        }
        let focused = self.core.borrow().focused();
        match focused {
            Some(FocusTarget::Overlay { id }) => {
                let Some(index) = self.core.borrow().overlay_index(id) else {
                    return;
                };
                // TS 的 `if (this.focusedComponent?.handleInput)`：没有输入处理器的聚焦组件
                // 既不消费输入，也不请求立即渲染。
                if !self.core.borrow().overlay_stack[index]
                    .component
                    .handles_input()
                {
                    return;
                }
                let wants_release = self.core.borrow().overlay_stack[index]
                    .component
                    .wants_key_release();
                if crate::keys::is_key_release(&current) && !wants_release {
                    return;
                }
                self.core.borrow_mut().overlay_stack[index]
                    .component
                    .handle_input(&current);
                // 键盘输入对延迟敏感：走立即路径，避开 16 ms 节流。
                self.request_immediate_render();
            }
            Some(
                target @ (FocusTarget::Component { .. }
                | FocusTarget::Path { .. }
                | FocusTarget::LayoutRoot { .. }),
            ) => {
                let mut handled = false;
                self.with_component_mut_of(target, &mut |component| {
                    if !component.handles_input() {
                        return;
                    }
                    if crate::keys::is_key_release(&current) && !component.wants_key_release() {
                        return;
                    }
                    component.handle_input(&current);
                    handled = true;
                });
                if !handled {
                    return;
                }
                self.request_immediate_render();
            }
            None => {}
        }
    }

    /// 按 [`FocusTarget`] 取出组件并对它执行回调（`Path`/`LayoutRoot` 会经
    /// [`Component::for_each_container_child_mut`] 递归下钻，因此能穿过 Stack/ScrollView 的
    /// 共享句柄代理）。
    fn with_component_mut_of(
        &mut self,
        target: FocusTarget,
        f: &mut dyn FnMut(&mut (dyn Component + 'static)),
    ) {
        match target {
            FocusTarget::Component { index } => {
                if let Some(child) = self.children.get_mut(index) {
                    f(child.as_mut());
                }
            }
            FocusTarget::Path { path } => {
                let indices = path.indices();
                if let Some((first, rest)) = indices.split_first()
                    && let Some(root) = self.children.get_mut(*first as usize)
                {
                    with_component_mut_at_path(root.as_mut(), rest, f);
                }
            }
            FocusTarget::LayoutRoot { path } => {
                if let Some(root) = self.layout_root.as_mut() {
                    with_component_mut_at_path(root.as_mut(), path.indices(), f);
                }
            }
            FocusTarget::Overlay { .. } => {}
        }
    }

    /// 输入处理第 6 步：聚焦的 overlay 不可见时退回最上层可见的捕获型 overlay
    /// （`topmost_visible_overlay` 跳过 `non_capturing`）或它的 `preFocus`。
    fn repair_invisible_focused_overlay(&mut self) {
        let focused = self.core.borrow().focused();
        let Some(FocusTarget::Overlay { id }) = focused else {
            return;
        };
        let focused_invisible = {
            let core = self.core.borrow();
            core.overlay_index(id)
                .is_some_and(|index| !core.is_overlay_visible(&core.overlay_stack[index]))
        };
        if !focused_invisible {
            return;
        }
        let mut core = self.core.borrow_mut();
        let top_visible = core.topmost_visible_overlay();
        let pre_focus = core
            .overlay_index(id)
            .and_then(|index| core.overlay_stack[index].pre_focus);
        let next = top_visible
            .map(|overlay| FocusTarget::Overlay { id: overlay })
            .or(pre_focus);
        core.set_focus_internal(next, OverlayFocusRestorePolicy::Preserve);
        drop(core);
        self.flush_pending_focus_sync();
    }

    /// 输入处理第 7 步：焦点不在 overlay 上时按**实时**恢复状态夺回焦点。
    fn restore_overlay_focus_before_input(&mut self) {
        let focused = self.core.borrow().focused();
        let focus_is_overlay = match focused {
            Some(FocusTarget::Overlay { id }) => self.core.borrow().overlay_index(id).is_some(),
            _ => false,
        };
        if focus_is_overlay {
            return;
        }
        let target = match self.core.borrow().visible_overlay_focus_restore() {
            OverlayFocusRestoreState::Eligible { overlay } => {
                Some(FocusTarget::Overlay { id: overlay })
            }
            OverlayFocusRestoreState::Blocked {
                overlay,
                blocked_by,
                resume,
            } => {
                if blocked_by == focused {
                    None
                } else {
                    match resume {
                        OverlayBlockedFocusResume::RestoreOverlay => {
                            Some(FocusTarget::Overlay { id: overlay })
                        }
                        OverlayBlockedFocusResume::FocusTarget { target } => {
                            // TS：`clearOverlayFocusRestore(); setFocus(restoreState.resume.target)`。
                            self.core.borrow_mut().clear_overlay_focus_restore();
                            target
                        }
                    }
                }
            }
            OverlayFocusRestoreState::Inactive => None,
        };
        if let Some(target) = target {
            self.set_focus(Some(target));
        }
    }

    /// 消费一条终端颜色回复（OSC 10/11/4）或 DA1，返回这段输入是否已被消费。
    ///
    /// 回复本身不携带查询 id，因此一律归给**队首**查询：颜色回复累积到队首条目，DA1
    /// 表示这一轮查询结束，把队首移出队列（后续查询要等它移出后才接收颜色回复）。
    /// 超时后到达的回复仍被消费但不再记录（除非该查询提供了 `onLateReply`）。
    fn consume_terminal_color_response(&mut self, data: &str) -> bool {
        use crate::terminal_colors::{OscColorTarget, parse_osc_color_response};
        {
            let mut pending = self.pending_terminal_color_queries.borrow_mut();
            if pending.is_empty() {
                return false;
            }
            if is_device_attributes_response(data) {
                let mut query = pending.remove(0);
                drop(pending);
                // 截止时刻已过（TS 的 `setTimeout` 已经 resolve 过 promise）：promise 用部分
                // 结果快照 resolve，DA1 带来的完整结果只交给 `onLateReply`。
                if query.deliver.is_some() && query.deadline_passed() {
                    let snapshot = query.result();
                    *query.snapshot.borrow_mut() = Some(snapshot.clone());
                    if let Some(deliver) = query.deliver.take() {
                        let _ = deliver.send(snapshot);
                    }
                }
                complete_terminal_color_query(&mut query);
                return true;
            }
            let Some((target, rgb)) = parse_osc_color_response(data) else {
                return false;
            };
            let key = match target {
                OscColorTarget::Foreground => TerminalColorTargetKey::Foreground,
                OscColorTarget::Background => TerminalColorTargetKey::Background,
                OscColorTarget::Palette(index) => TerminalColorTargetKey::Palette(index),
            };
            // 颜色回复不带查询 id：只归给队首查询（FIFO）。
            let query = &mut pending[0];
            // 截止时刻已过：补做 TS `setTimeout` 回调的状态切换（记录部分结果快照、丢弃 promise
            // 投递端），本条回复从此只进 `onLateReply`。
            query.timeout_if_due();
            // TS `if (!query.deliver || query.replied.has(key)) return true`：超时且没有
            // onLateReply 时，迟到回复仍被消费（不会变成用户输入），但不再记录。
            if (query.deliver.is_none() && query.late.is_none()) || query.replied.contains(&key) {
                return true;
            }
            query.replied.insert(key);
            match (target, rgb) {
                (OscColorTarget::Foreground, Some(rgb)) => query.foreground = Some(rgb),
                (OscColorTarget::Background, Some(rgb)) => query.background = Some(rgb),
                (OscColorTarget::Palette(index), Some(rgb)) if index < TERMINAL_PALETTE_SIZE => {
                    query.palette[index as usize] = Some(rgb);
                }
                _ => {}
            }
            if query.replied.len() != TERMINAL_COLOR_REPLY_COUNT {
                return true;
            }
            // TS `if (query.replied.size === TERMINAL_COLOR_REPLY_COUNT) completeTerminalColorQuery(query)`
            // ——**不移出队列**：队列项留到 DA1 到达时才 `shift()`。完成时 `deliver`/`late` 被取走，
            // 因此其后的颜色回复被"消费但忽略"，DA1 到达时的第二次完成是空操作。
            complete_terminal_color_query(query);
        }
        true
    }

    /// 解析终端明暗方案上报（`CSI ?997;1n` 为暗、`?997;2n` 为亮，整段可含多个片段），
    /// 同步通知所有监听器；命中格式即消费这段输入。
    fn consume_terminal_color_scheme_report(&mut self, data: &str) -> bool {
        let Some(scheme) = crate::terminal_colors::parse_terminal_color_scheme_report(data) else {
            return false;
        };
        for (_, listener) in &mut self.terminal_color_scheme_listeners {
            listener(scheme);
        }
        true
    }

    /// `consumeCellSizeResponse(data)`（`tui.ts:1217-1242`）：`CSI 6;h;w t` → `setCellDimensions`。
    ///
    /// 无论是否有 `setCellDimensions` 的接收方，命中格式就**消费**掉这段输入
    /// （不会落到聚焦组件），随后失效全部组件并请求渲染；尺寸为 0 时消费但忽略。
    fn consume_cell_size_response(&mut self, data: &str) -> bool {
        let Some((height_px, width_px)) = parse_cell_size_response(data) else {
            return false;
        };
        if height_px == 0 || width_px == 0 {
            return true;
        }
        // TS 无条件调用 `setCellDimensions`；默认接收方即 `terminal_image::set_cell_dimensions`
        // （宿主可覆盖 `cell_size_handler` 观察/拦截，未设置时忽略尺寸）。
        if let Some(handler) = self.cell_size_handler.as_mut() {
            handler(width_px, height_px);
        }
        self.invalidate_all();
        self.request_render(false);
        true
    }

    /// `queryTerminalColors({timeoutMs, onLateReply})`。
    ///
    /// **发起**查询（写入 `TERMINAL_COLOR_QUERY`）并返回一个**不借用 `self`** 的 future：
    /// TS 的 promise 同样不阻塞事件循环，宿主可以先继续喂输入（`handleTerminalInput` 会用共享
    /// 队列消费回复），再 `await` 这个 future。截止时刻在**调用时**确定（TS 的 `setTimeout`
    /// 就在 `queryTerminalColors` 里创建）；超时时 future 以**截止时刻的部分结果** resolve，
    /// 之后到达的颜色回复继续被记录，DA1（或收齐 16 色）时以累积结果调用 `onLateReply`
    /// （`tui.ts:1520-1542`、`1159-1203`）。并发查询按 id 精确超时，互不串数据。
    ///
    /// 但颜色/DA1 **回复不带查询 id**，只能按 FIFO 归给队首查询（见
    /// `TuiBase::consume_terminal_color_response`）：并发查询时，后一个要等前一个被 DA1
    /// 移出队列后才开始累积自己的颜色；超时不会把队首移出队列。
    pub fn query_terminal_colors_with_late_reply(
        &mut self,
        timeout_ms: u64,
        late: Option<Box<dyn FnMut(TerminalColors)>>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = TerminalColors> + 'static>> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        // 计时从**调用**开始（TS `query.timer = setTimeout(..., timeoutMs)` 与查询同一个同步块）。
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        self.next_color_query_id += 1;
        let id = self.next_color_query_id;
        let pending = Rc::clone(&self.pending_terminal_color_queries);
        let snapshot: Rc<RefCell<Option<TerminalColors>>> = Rc::new(RefCell::new(None));
        pending.borrow_mut().push(PendingTerminalColorQuery {
            id,
            foreground: None,
            background: None,
            palette: vec![None; TERMINAL_PALETTE_SIZE as usize],
            replied: std::collections::HashSet::new(),
            deliver: Some(sender),
            late,
            deadline,
            snapshot: Rc::clone(&snapshot),
        });
        self.terminal.borrow_mut().write(TERMINAL_COLOR_QUERY);
        Box::pin(async move {
            match tokio::time::timeout_at(deadline, receiver).await {
                Ok(Ok(colors)) => colors,
                _ => {
                    // 超时（或在截止时刻之后才完成的投递端被丢弃）：promise 在截止时刻就已
                    // resolve（TS `setTimeout`），优先返回共享快照。
                    let early = snapshot.borrow().clone();
                    if let Some(colors) = early {
                        return colors;
                    }
                    // 只改**这个**查询（按 id），返回截止时刻的部分结果快照。
                    let mut pending = pending.borrow_mut();
                    match pending.iter_mut().find(|query| query.id == id) {
                        Some(query) => {
                            query.deliver = None;
                            let result = query.result();
                            *query.snapshot.borrow_mut() = Some(result.clone());
                            result
                        }
                        // 理论上不可达：查询项被移出队列前一定已写入快照（DA1 超时分支或
                        // `timeout_if_due`）。
                        None => TerminalColors::default(),
                    }
                }
            }
        })
    }

    /// 单参数版本（[`Tui::query_terminal_colors`] 的冻结签名）：没有 onLateReply。
    pub fn query_terminal_colors(
        &mut self,
        timeout_ms: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = TerminalColors> + '_>> {
        self.query_terminal_colors_with_late_reply(timeout_ms, None)
    }

    /// 当前未完成的颜色查询数（测试/调试用）。
    pub fn pending_terminal_color_query_count(&self) -> usize {
        self.pending_terminal_color_queries.borrow().len()
    }

    /// `compositeOverlays(lines, termWidth, termHeight)`（`spec/tui-core.md` §2.9.12）。
    pub fn composite_overlays(
        &mut self,
        lines: Vec<String>,
        term_width: usize,
        term_height: usize,
    ) -> Vec<String> {
        // 渲染入口：overlay handle 可能刚改过焦点（TS 的 handle 同步改组件属性），先刷新
        // `focused` 标志，保证本次渲染（含 CURSOR_MARKER 的选择）看到与 TS 相同的状态。
        self.flush_pending_focus_sync();
        if !self.core.borrow().has_overlay_entries() {
            self.core
                .borrow_mut()
                .set_rendered_overlay_layouts(Vec::new());
            return lines;
        }
        let mut result = lines;
        self.core.borrow_mut().reset_overlay_bounds();

        struct Rendered {
            id: usize,
            overlay_lines: Vec<String>,
            row: usize,
            col: isize,
            width: usize,
        }

        let mut rendered: Vec<Rendered> = Vec::new();
        let mut min_lines_needed = result.len();

        // 阶段 1：收集可见 overlay 并按 focus_order 升序排列。合成时按该顺序叠加，
        // focus_order 更大的最后画，因此视觉上位于最前。
        let mut visible_ids: Vec<(usize, u64)> = {
            let core = self.core.borrow();
            core.overlay_stack
                .iter()
                .filter(|entry| core.is_overlay_visible(entry))
                .map(|entry| (entry.id, entry.focus_order))
                .collect()
        };
        visible_ids.sort_by_key(|(_, focus_order)| *focus_order);

        // 阶段 2：逐个渲染 overlay。第一次布局解析只需要 width/max_height（高度传 0），
        // 等实际渲染出行数后，再用真实高度解析 row/col。
        for (id, _) in visible_ids {
            let (width, max_height) = {
                let core = self.core.borrow();
                let index = core.overlay_index(id).expect("可见 overlay 在栈中");
                let layout = resolve_overlay_layout(
                    core.overlay_stack[index].options.as_ref(),
                    0,
                    term_width,
                    term_height,
                );
                (layout.width, layout.max_height)
            };
            // TS 把 width 原样交给 `component.render(width)`（可能带小数）；Rust 的渲染宽度
            // 是 usize，按 JS 的下标/长度语义向零取整。
            let render_width = if width.is_finite() && width > 0.0 {
                width.trunc() as usize
            } else {
                0
            };
            let render_result = {
                let mut core = self.core.borrow_mut();
                let index = core.overlay_index(id).expect("可见 overlay 在栈中");
                core.overlay_stack[index].component.render(render_width)
            };
            let mut overlay_lines = self.resolve_fake_cursors(render_result);
            // `overlayLines.slice(0, maxHeight)`：`slice` 对小数做 ToIntegerOrInfinity（截断）。
            if let Some(max_height) = max_height {
                let max_height = if max_height.is_finite() && max_height > 0.0 {
                    max_height.trunc() as usize
                } else {
                    0
                };
                if overlay_lines.len() > max_height {
                    overlay_lines.truncate(max_height);
                }
            }
            let (row_f, col_f) = {
                let core = self.core.borrow();
                let index = core.overlay_index(id).expect("可见 overlay 在栈中");
                let layout = resolve_overlay_layout(
                    core.overlay_stack[index].options.as_ref(),
                    overlay_lines.len(),
                    term_width,
                    term_height,
                );
                (layout.row, layout.col)
            };
            let row = if row_f.is_finite() && row_f > 0.0 {
                row_f.trunc() as usize
            } else {
                0
            };
            let col = if col_f.is_finite() {
                col_f.trunc() as isize
            } else {
                0
            };
            let width = if width.is_finite() && width > 0.0 {
                width.trunc() as usize
            } else {
                0
            };
            {
                let mut core = self.core.borrow_mut();
                let index = core.overlay_index(id).expect("可见 overlay 在栈中");
                core.overlay_stack[index].bounds = Some(OverlayBounds {
                    row,
                    col: col.max(0) as usize,
                    width,
                    height: overlay_lines.len(),
                });
            }
            min_lines_needed = min_lines_needed.max(row + overlay_lines.len());
            rendered.push(Rendered {
                id,
                overlay_lines,
                row,
                col,
                width,
            });
        }

        self.core.borrow_mut().set_rendered_overlay_layouts(
            rendered
                .iter()
                .map(|entry| RenderedOverlayLayout {
                    id: entry.id,
                    row: entry.row,
                    // `row`/`col` 已在上面按单元格向零取整；命中测试的坐标本来就是整格。
                    col: entry.col.max(0) as usize,
                    width: entry.width,
                    height: entry.overlay_lines.len(),
                })
                .collect(),
        );

        // 阶段 3：把画布补到至少覆盖终端高度与最低的 overlay 底边。overlay 的 row 相对
        // "底对齐视口"计算（`viewport_start = 总高 - 终端高`），而不是相对整块画布顶部。
        let working_height = result.len().max(term_height).max(min_lines_needed);
        while result.len() < working_height {
            result.push(String::new());
        }
        let viewport_start = working_height.saturating_sub(term_height);

        // 按 focus_order 升序叠加：后合成的行会覆盖先合成行在同一列区间的内容。
        for entry in &rendered {
            for (index, overlay_line) in entry.overlay_lines.iter().enumerate() {
                let target = viewport_start + entry.row + index;
                if target >= result.len() {
                    continue;
                }
                let truncated = if crate::text::visible_width(overlay_line) > entry.width {
                    crate::text::slice_by_column(overlay_line, 0, entry.width, true)
                } else {
                    overlay_line.clone()
                };
                result[target] = composite_tui_line(
                    &result[target],
                    &truncated,
                    entry.col,
                    entry.width,
                    term_width,
                );
            }
        }
        result
    }

    /// `dispatchMouseToOverlay(event)`：按最近一次渲染的 overlay 边界做命中测试。
    ///
    /// 从视觉最前（`focus_order` 最大，即 `rendered_overlay_layouts` 的末尾）往前找，第一个
    /// 包含 `screen_x`/`screen_y` 的 overlay 会把事件改写成局部坐标后交给
    /// [`dispatch_mouse_event`]。返回 `(是否命中 overlay 区域, 分发结果)`：命中即消费这次
    /// 鼠标事件，即使组件返回 `None`（结果也为 `None`），调用方不应再把它交给普通子项。
    pub fn dispatch_mouse_to_overlay(
        &mut self,
        event: &TuiMouseEvent,
    ) -> (bool, Option<TuiMouseDispatchResult>) {
        // 鼠标入口：先把 handle 路径挂起的 `focused` 标志刷到组件（同 TS 的同步语义）。
        self.flush_pending_focus_sync();
        let layouts = self.core.borrow().rendered_overlay_layouts().to_vec();
        for layout in layouts.iter().rev() {
            if event.screen_x < layout.col as isize
                || event.screen_x >= (layout.col + layout.width) as isize
                || event.screen_y < layout.row as isize
                || event.screen_y >= (layout.row + layout.height) as isize
            {
                continue;
            }
            let mut core = self.core.borrow_mut();
            let Some(index) = core.overlay_index(layout.id) else {
                continue;
            };
            let mut component_event = event.clone();
            component_event.x = event.screen_x - layout.col as isize;
            component_event.y = event.screen_y - layout.row as isize;
            component_event.width = layout.width;
            component_event.height = layout.height;
            let result = dispatch_mouse_event(
                core.overlay_stack[index].component.as_mut(),
                &component_event,
            );
            return match result {
                Some(result) => {
                    let focus_target = if result.focus == Some(true) {
                        Some(FocusTarget::Overlay { id: layout.id })
                    } else {
                        None
                    };
                    (
                        true,
                        Some(TuiMouseDispatchResult {
                            handled: true,
                            target: TuiMouseDispatchTarget {
                                origin_x: layout.col as isize,
                                origin_y: layout.row as isize,
                                width: layout.width,
                                height: layout.height,
                            },
                            focus_target,
                        }),
                    )
                }
                None => (true, None),
            };
        }
        (false, None)
    }

    /// `applyLineResets(lines)`：非图片行补 `normalizeTerminalOutput(line) + SEGMENT_RESET`。
    pub fn apply_line_resets(&self, mut lines: Vec<String>) -> Vec<String> {
        for line in &mut lines {
            if !crate::terminal_image::is_image_line(line) {
                *line = format!(
                    "{}{SEGMENT_RESET}",
                    crate::text::normalize_terminal_output(line)
                );
            }
        }
        lines
    }

    /// `resolveFakeCursors(lines)`：伪光标标记 → 反显；显示硬件光标时丢掉聚焦光标处的伪光标。
    pub fn resolve_fake_cursors(&self, lines: Vec<String>) -> Vec<String> {
        lines
            .into_iter()
            .map(|line| {
                if !line.contains(FAKE_CURSOR_START) {
                    return line;
                }
                let resolved = if self.show_hardware_cursor {
                    strip_focused_fake_cursor(&line)
                } else {
                    line
                };
                resolved
                    .replace(FAKE_CURSOR_START, "\x1b[7m")
                    .replace(FAKE_CURSOR_END, "\x1b[27m")
            })
            .collect()
    }

    /// `extractCursorPosition(lines, height)`：找最后一个 `CURSOR_MARKER`，把它剥掉并返回位置。
    pub fn extract_cursor_position(
        &self,
        lines: &mut [String],
        height: usize,
    ) -> Option<(usize, usize)> {
        let viewport_top = lines.len().saturating_sub(height);
        for row in (viewport_top..lines.len()).rev() {
            let Some(marker_index) = lines[row].find(CURSOR_MARKER) else {
                continue;
            };
            let before = &lines[row][..marker_index];
            let col = crate::text::visible_width(before);
            let mut next = String::with_capacity(lines[row].len());
            next.push_str(before);
            next.push_str(&lines[row][marker_index + CURSOR_MARKER.len()..]);
            lines[row] = next;
            return Some((row, col));
        }
        None
    }
}

/// `line.replace(FOCUSED_FAKE_CURSOR, "$1$2")`：**只替换第一处**。
///
/// TS 的 `FOCUSED_FAKE_CURSOR` 是没有 `g` 标志的正则：
/// `/(\x1b_kiana:c\x07)\x1b_kiana:fc\x07(.*?)(?:\x1b_kiana:\/fc\x07|$)/s`。
/// `String.prototype.replace` 的非全局形式只替换**最左边的一处**匹配：正则引擎从左到右
/// 逐个起点尝试，因此匹配点是"第一个紧跟 `FAKE_CURSOR_START` 的 `CURSOR_MARKER`"——前面
/// 那些后面不跟伪光标的 marker 会被跳过，但匹配**之后**的伪光标对不会被处理。
fn strip_focused_fake_cursor(line: &str) -> String {
    let mut search_from = 0;
    let start = loop {
        let Some(relative) = line[search_from..].find(CURSOR_MARKER) else {
            return line.to_string();
        };
        let start = search_from + relative;
        let after_marker = start + CURSOR_MARKER.len();
        if line[after_marker..].starts_with(FAKE_CURSOR_START) {
            break start;
        }
        search_from = after_marker;
    };
    let after_marker = start + CURSOR_MARKER.len();
    let body_start = after_marker + FAKE_CURSOR_START.len();
    let body_rest = &line[body_start..];
    let end = body_rest.find(FAKE_CURSOR_END).unwrap_or(body_rest.len());
    let mut result = String::with_capacity(line.len());
    result.push_str(&line[..after_marker]);
    result.push_str(&body_rest[..end]);
    if end < body_rest.len() {
        result.push_str(&body_rest[end + FAKE_CURSOR_END.len()..]);
    }
    result
}

/// `DEVICE_ATTRIBUTES_RESPONSE_PATTERN = /^\x1b\[\?[\d;]*c$/`。
fn is_device_attributes_response(data: &str) -> bool {
    let Some(body) = data.strip_prefix("\x1b[?") else {
        return false;
    };
    let Some(body) = body.strip_suffix('c') else {
        return false;
    };
    body.bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b';')
}

/// `/^\x1b\[6;(\d+);(\d+)t$/` → `(heightPx, widthPx)`。
///
/// JS 的 `parseInt` 对任意长度的数字串都成功（超出 i64 时得到大浮点数），因此这里**饱和**
/// 而不是拒绝：`usize::MAX` 是"极大的正数"在单元格尺寸语境下的等价物。
fn parse_cell_size_response(data: &str) -> Option<(usize, usize)> {
    let body = data.strip_prefix("\x1b[6;")?.strip_suffix('t')?;
    let (height, width) = body.split_once(';')?;
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    if !digits(height) || !digits(width) {
        return None;
    }
    let parse = |text: &str| -> usize {
        let mut value: usize = 0;
        for byte in text.bytes() {
            value = value
                .saturating_mul(10)
                .saturating_add(usize::from(byte - b'0'));
        }
        value
    };
    Some((parse(height), parse(width)))
}

/// overlay 布局解析结果（TS `resolveOverlayLayout` 的返回值）。
///
/// TS 全是 `number`（f64），并且绝对值的 `width`/`row`/`col` 与小数 `offsetX`/`offsetY`
/// 会**保留小数**（只有百分比走 `Math.floor`）。Rust 侧同样用 f64 保存几何；调用方在把
/// 它用于 `Array.prototype.slice` 语义（`maxHeight`）或下标/列宽时再按 TS 规则取整：
/// `maxHeight` 走 `Math.trunc`（`slice` 会 ToIntegerOrInfinity），`row`/`col`/`width`
/// 在下标/列运算里同样向零取整（[`ResolvedOverlayLayout::truncated`] 提供统一入口）。
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedOverlayLayout {
    /// 列宽（单元格，可能带小数）。
    pub width: f64,
    /// 行号（已夹回上下边距内，保留小数）。
    pub row: f64,
    /// 列号（已夹回左右边距内，保留小数）。
    pub col: f64,
    /// 解析并夹取后的高度上限；`None` 表示调用方没有设置 `max_height`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_height: Option<f64>,
}

impl ResolvedOverlayLayout {
    /// 用于矩阵下标/列宽截断的几何（JS 的 `ToIntegerOrInfinity` = 向零取整）。
    pub fn truncated(&self) -> (usize, isize, usize, Option<usize>) {
        let to_index = |value: f64| -> usize {
            if !value.is_finite() || value <= 0.0 {
                0
            } else {
                value.trunc() as usize
            }
        };
        let col = if self.col.is_finite() {
            self.col.trunc() as isize
        } else {
            0
        };
        (
            to_index(self.width),
            col,
            to_index(self.row),
            self.max_height.map(to_index),
        )
    }
}

fn margin_values(options: Option<&OverlayOptions>) -> (f64, f64, f64, f64) {
    let margin = options.and_then(|options| options.margin);
    let (top, right, bottom, left) = match margin {
        Some(OverlayMarginValue::All(value)) => (value, value, value, value),
        Some(OverlayMarginValue::Sides(sides)) => (
            sides.top.unwrap_or(0.0),
            sides.right.unwrap_or(0.0),
            sides.bottom.unwrap_or(0.0),
            sides.left.unwrap_or(0.0),
        ),
        None => (0.0, 0.0, 0.0, 0.0),
    };
    (top.max(0.0), right.max(0.0), bottom.max(0.0), left.max(0.0))
}

fn resolve_anchor_row(
    anchor: OverlayAnchor,
    height: f64,
    avail_height: f64,
    margin_top: f64,
) -> f64 {
    match anchor {
        OverlayAnchor::TopLeft | OverlayAnchor::TopCenter | OverlayAnchor::TopRight => margin_top,
        OverlayAnchor::BottomLeft | OverlayAnchor::BottomCenter | OverlayAnchor::BottomRight => {
            margin_top + avail_height - height
        }
        OverlayAnchor::LeftCenter | OverlayAnchor::Center | OverlayAnchor::RightCenter => {
            margin_top + ((avail_height - height) / 2.0).floor()
        }
    }
}

fn resolve_anchor_col(
    anchor: OverlayAnchor,
    width: f64,
    avail_width: f64,
    margin_left: f64,
) -> f64 {
    match anchor {
        OverlayAnchor::TopLeft | OverlayAnchor::LeftCenter | OverlayAnchor::BottomLeft => {
            margin_left
        }
        OverlayAnchor::TopRight | OverlayAnchor::RightCenter | OverlayAnchor::BottomRight => {
            margin_left + avail_width - width
        }
        OverlayAnchor::TopCenter | OverlayAnchor::Center | OverlayAnchor::BottomCenter => {
            margin_left + ((avail_width - width) / 2.0).floor()
        }
    }
}

/// `resolveOverlayLayout(options, overlayHeight, termWidth, termHeight)`。
///
/// 语义（全部用 f64 保留 TS 的小数）：
/// - 可用区域 = 终端尺寸减去四边 margin，宽高各自至少为 1；
/// - 宽度：`width` 解析（百分比按终端宽）→ `min_width` 下限 → 夹到 `[1, 可用宽度]`；
///   未指定时取 `min(80, 可用宽度)`；
/// - 高度：`max_height` 解析后夹到 `[1, 可用高度]`；实际渲染高度取
///   `min(overlayHeight, max_height)`；
/// - 位置：显式 `row`/`col` 优先（百分比相对"可用空间减去 overlay 尺寸"后的余量，向下
///   取整），否则用锚点；之后叠加 offset，最后夹回边距内；
/// - 返回值保留小数，取整由调用方按使用场景完成（[`ResolvedOverlayLayout::truncated`]
///   提供统一入口）。
pub fn resolve_overlay_layout(
    options: Option<&OverlayOptions>,
    overlay_height: usize,
    term_width: usize,
    term_height: usize,
) -> ResolvedOverlayLayout {
    let (margin_top, margin_right, margin_bottom, margin_left) = margin_values(options);
    let term_width = term_width as f64;
    let term_height = term_height as f64;
    let avail_width = (term_width - margin_left - margin_right).max(1.0);
    let avail_height = (term_height - margin_top - margin_bottom).max(1.0);

    let width = options
        .and_then(|options| options.width)
        .map(|value| value.resolve(term_width))
        .unwrap_or_else(|| 80.0f64.min(avail_width));
    let width = match options.and_then(|options| options.min_width) {
        Some(min_width) => width.max(min_width),
        None => width,
    };
    let width = width.max(1.0).min(avail_width);

    let max_height = options
        .and_then(|options| options.max_height)
        .map(|value| value.resolve(term_height))
        .map(|value| value.max(1.0).min(avail_height));

    let effective_height = match max_height {
        Some(max_height) => (overlay_height as f64).min(max_height),
        None => overlay_height as f64,
    };

    let anchor = options
        .and_then(|options| options.anchor)
        .unwrap_or(OverlayAnchor::Center);

    let mut row = match options.and_then(|options| options.row) {
        // 百分比：0% = 顶部，100% = 底部（overlay 保持在界内）。
        //
        // 运算顺序固定（`tui.ts:1272-1275`）：先 `parseFloat(p) / 100` 再乘，最后
        // `Math.floor`。写成 `maxRow * percent / 100.0` 会在浮点上多舍入一次——例如
        // `50 * 0.58` 在 JS 里是 28.999999999999996（floor 28），而 `50 * 58 / 100` 恰好是 29。
        Some(SizeValue::Percent(percent)) => {
            let max_row = (avail_height - effective_height).max(0.0);
            margin_top + (max_row * (percent / 100.0)).floor()
        }
        Some(SizeValue::Absolute(value)) => value,
        None => resolve_anchor_row(anchor, effective_height, avail_height, margin_top),
    };
    let mut col = match options.and_then(|options| options.col) {
        Some(SizeValue::Percent(percent)) => {
            let max_col = (avail_width - width).max(0.0);
            margin_left + (max_col * (percent / 100.0)).floor()
        }
        Some(SizeValue::Absolute(value)) => value,
        None => resolve_anchor_col(anchor, width, avail_width, margin_left),
    };

    if let Some(offset_y) = options.and_then(|options| options.offset_y) {
        row += offset_y;
    }
    if let Some(offset_x) = options.and_then(|options| options.offset_x) {
        col += offset_x;
    }

    row = margin_top.max(row.min(term_height - margin_bottom - effective_height));
    col = margin_left.max(col.min(term_width - margin_right - width));

    ResolvedOverlayLayout {
        width,
        row,
        col,
        max_height,
    }
}

/// 终端调色板大小（`TERMINAL_PALETTE_SIZE`）。
pub const TERMINAL_PALETTE_SIZE: u16 = 16;
/// `TERMINAL_COLOR_REPLY_COUNT = 2 + 16`。
const TERMINAL_COLOR_REPLY_COUNT: usize = 2 + TERMINAL_PALETTE_SIZE as usize;
/// `TERMINAL_COLOR_QUERY`：OSC 10 + OSC 11 + 16 条 OSC 4 + DA1。
pub const TERMINAL_COLOR_QUERY: &str = concat!(
    "\x1b]10;?\x07\x1b]11;?\x07",
    "\x1b]4;0;?\x07\x1b]4;1;?\x07\x1b]4;2;?\x07\x1b]4;3;?\x07\x1b]4;4;?\x07\x1b]4;5;?\x07\x1b]4;6;?\x07\x1b]4;7;?\x07",
    "\x1b]4;8;?\x07\x1b]4;9;?\x07\x1b]4;10;?\x07\x1b]4;11;?\x07\x1b]4;12;?\x07\x1b]4;13;?\x07\x1b]4;14;?\x07\x1b]4;15;?\x07",
    "\x1b[c",
);
/// `MIN_RENDER_INTERVAL_MS`（TuiBase 的渲染节流窗口）。
pub const MIN_RENDER_INTERVAL_MS: f64 = 16.0;

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(Vec<String>);

    impl Component for Fixed {
        fn render(&mut self, _width: usize) -> Vec<String> {
            self.0.clone()
        }
    }

    #[test]
    fn container_stacks_children_and_routes_mouse_by_height() {
        let mut container = Container::new();
        container.add_child(Box::new(Fixed(vec!["a".into(), "b".into()])));
        container.add_child(Box::new(Fixed(vec!["c".into()])));
        assert_eq!(container.render(10), vec!["a", "b", "c"]);
        // 缓存项是稳定的子项 id（1、2），不是下标；见 `Container` 的文档。
        assert_eq!(
            container.mouse_layout.as_ref().unwrap().1,
            vec![(1, 2), (2, 1)]
        );
    }

    #[test]
    fn size_value_percent_floors() {
        assert_eq!(SizeValue::Percent(50.0).resolve(7.0), 3.0);
        assert_eq!(SizeValue::Absolute(4.0).resolve(7.0), 4.0);
    }

    // ------------------------------------------------------------------
    // 回归测试：见各测试的注释（对应 review 报告里 tui.rs 的偏差项）。
    // ------------------------------------------------------------------

    /// 只够驱动 `TuiBase` 的终端桩：记录写入，固定尺寸，Kitty 协议始终激活。
    struct StubTerminal {
        columns: usize,
        rows: usize,
        output: Rc<RefCell<String>>,
    }

    impl StubTerminal {
        fn new(columns: usize, rows: usize) -> Self {
            StubTerminal {
                columns,
                rows,
                output: Rc::new(RefCell::new(String::new())),
            }
        }
    }

    impl Terminal for StubTerminal {
        fn start(&mut self, _on_input: Box<dyn FnMut(&str)>, _on_resize: Box<dyn FnMut()>) {}
        fn stop(&mut self) {}
        fn drain_input(
            &mut self,
            _max_ms: u64,
            _idle_ms: u64,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + '_>> {
            Box::pin(async {})
        }
        fn write(&mut self, data: &str) {
            self.output.borrow_mut().push_str(data);
        }
        fn columns(&self) -> usize {
            self.columns
        }
        fn rows(&self) -> usize {
            self.rows
        }
        fn kitty_protocol_active(&self) -> bool {
            true
        }
        fn move_by(&mut self, _lines: isize) {}
        fn hide_cursor(&mut self) {}
        fn show_cursor(&mut self) {}
        fn clear_line(&mut self) {}
        fn clear_from_cursor(&mut self) {}
        fn clear_screen(&mut self) {}
        fn set_title(&mut self, _title: &str) {}
        fn set_progress(&mut self, _active: bool) {}
        fn set_program_status(&mut self, _status: &crate::terminal_colors::ProgramStatus) {}
    }

    /// `focused` 标志的共享句柄。
    type FocusFlag = Rc<std::cell::Cell<bool>>;
    /// 输入记录（`handle_input` 收到的原始数据）。
    type InputLog = Rc<RefCell<Vec<String>>>;

    /// 可聚焦的探针：渲染一行、记录输入、鼠标按下即请求焦点。
    struct FocusProbe {
        focused: FocusFlag,
        inputs: InputLog,
    }

    impl FocusProbe {
        fn new() -> (Self, FocusFlag, InputLog) {
            let focused = Rc::new(std::cell::Cell::new(false));
            let inputs = Rc::new(RefCell::new(Vec::new()));
            (
                FocusProbe {
                    focused: Rc::clone(&focused),
                    inputs: Rc::clone(&inputs),
                },
                focused,
                inputs,
            )
        }
    }

    impl Component for FocusProbe {
        fn render(&mut self, _width: usize) -> Vec<String> {
            vec!["probe".to_string()]
        }
        fn handle_input(&mut self, data: &str) {
            self.inputs.borrow_mut().push(data.to_string());
        }
        fn handle_mouse(&mut self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
            Some(TuiMouseEventResult {
                focus: Some(true),
                ..TuiMouseEventResult::default()
            })
        }
        fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
            Some(self)
        }
    }

    impl Focusable for FocusProbe {
        fn focused(&self) -> bool {
            self.focused.get()
        }
        fn set_focused(&mut self, focused: bool) {
            self.focused.set(focused);
        }
    }

    /// 模拟 components 单元的 `Stack`/`ScrollView`：包装一个 [`Container`]，用
    /// [`Component::container_children`] 暴露它的子项（TS 里它们 `extends Container`）。
    struct ContainerWrapper {
        container: Container,
    }

    impl Component for ContainerWrapper {
        fn render(&mut self, width: usize) -> Vec<String> {
            self.container.render(width)
        }
        fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
            self.container.handle_mouse(event)
        }
        fn handles_input(&self) -> bool {
            // TS 的 Stack/ScrollView 没有 `handleInput`。
            false
        }
        fn container_children(&self) -> Option<&[Box<dyn Component>]> {
            Some(&self.container.children)
        }
        fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
            Some(&mut self.container.children)
        }
    }

    fn press_at(y: isize) -> TuiMouseEvent {
        TuiMouseEvent {
            event_type: TuiMouseEventType::Press,
            button: TuiMouseButton::Left,
            x: 0,
            y,
            screen_x: 0,
            screen_y: y,
            width: 10,
            height: 5,
            shift: false,
            alt: false,
            ctrl: false,
            wheel_delta: None,
            click_count: None,
        }
    }

    /// Review 报告（critical）：TS 的 `Container` 没有 `handleInput`，`Container.handleMouse`
    /// 返回的结果里 `focusTarget` 是**命中的子控件**（`tui.ts:383-403`），`applyMouseDispatchResult`
    /// 用 `result.target.component` 聚焦它。此前 Rust 的 `Container::handles_input` 继承了默认
    /// `true`，把焦点收给了容器本身（其 `handle_input` 是空实现，按键被吞掉）。
    #[test]
    fn container_mouse_focus_goes_to_the_hit_child_not_the_container() {
        let (probe, focused, inputs) = FocusProbe::new();
        let mut container = Container::new();
        container.add_child(Box::new(probe));
        let child_address =
            container.children[0].as_ref() as *const dyn Component as *const () as usize;
        assert!(
            !container.handles_input(),
            "TS 的 Container 没有 handleInput"
        );

        let result = container.handle_mouse(&press_at(0)).expect("命中子控件");
        assert_eq!(result.focus, Some(true));
        assert_eq!(
            result.focus_target,
            Some(MouseFocusTarget::Address(child_address)),
            "focusTarget 必须是命中的子控件，而不是 Container"
        );

        let mut base = TuiBase::new(Box::new(StubTerminal::new(80, 24)), None, None);
        base.add_child(Box::new(container));
        // 备用屏渲染器传的 fallback 是分发起点（Container 本身）；真正的目标来自结果里的地址。
        let changed = base.apply_mouse_focus_result(&result, FocusTarget::Component { index: 0 });
        assert!(changed);
        assert_eq!(
            base.focused_component(),
            Some(FocusTarget::Path {
                path: FocusPath::new(&[0, 0]).expect("非空路径")
            })
        );
        assert!(focused.get(), "子控件应拿到 focused 标志");
        base.handle_terminal_input("x");
        assert_eq!(
            *inputs.borrow(),
            vec!["x".to_string()],
            "按键必须到达子控件"
        );
    }

    /// Review 报告（major）：TS 的 row/col 百分比先除后乘
    /// （`margin + Math.floor(maxRow * (parseFloat(p) / 100))`，`tui.ts:1272-1275`），
    /// 与 `parseSizeValue` 的"先乘后除"不同。`50 * 0.58` 在 JS 里是 28.999999999999996。
    #[test]
    fn overlay_percent_position_divides_before_multiplying_like_js() {
        let options = OverlayOptions {
            row: Some(SizeValue::Percent(58.0)),
            ..OverlayOptions::default()
        };
        let layout = resolve_overlay_layout(Some(&options), 1, 100, 51);
        assert_eq!(layout.row, 28.0, "Math.floor(50 * (58 / 100)) === 28");

        let options = OverlayOptions {
            width: Some(SizeValue::Absolute(1.0)),
            col: Some(SizeValue::Percent(29.0)),
            ..OverlayOptions::default()
        };
        let layout = resolve_overlay_layout(Some(&options), 1, 101, 40);
        assert_eq!(layout.col, 28.0, "Math.floor(100 * (29 / 100)) === 28");

        // `parseSizeValue` 仍然是先乘后除：91 * 70 / 100 === 63.7 → 63。
        let options = OverlayOptions {
            max_height: Some(SizeValue::Percent(70.0)),
            ..OverlayOptions::default()
        };
        let layout = resolve_overlay_layout(Some(&options), 1, 100, 91);
        assert_eq!(layout.max_height, Some(63.0));
    }

    /// Review 报告（major）：TS 里 `TuiBase`/`Stack`/`ScrollView` 都继承 `Container`，
    /// 所以 `setLayoutRoot(new VStack([...editor...]))` 之后 `setFocus(editor)` 仍能收到按键
    /// （`test/tui-alt-screen.test.ts:780-810`）。Rust 之前既不能表达布局根内的目标，也不能
    /// 穿过 `Stack`/`ScrollView`（不是具体 `Container`）。
    #[test]
    fn focus_reaches_components_inside_the_layout_root_through_wrappers() {
        let (probe, focused, inputs) = FocusProbe::new();
        let mut wrapper = ContainerWrapper {
            container: Container::new(),
        };
        wrapper.container.add_child(Box::new(probe));

        let mut base = TuiBase::new(Box::new(StubTerminal::new(80, 24)), None, None);
        base.set_layout_root(Some(Box::new(wrapper)));
        let target = FocusTarget::LayoutRoot {
            path: FocusPath::new(&[0]).expect("非空路径"),
        };
        base.set_focus(Some(target));
        assert_eq!(base.focused_component(), Some(target));
        assert!(focused.get(), "布局根内的组件应拿到 focused 标志");

        base.handle_terminal_input("x");
        assert_eq!(
            *inputs.borrow(),
            vec!["x".to_string()],
            "按键必须到达布局根内的组件"
        );

        // 地址 → 目标的解析（鼠标分发用）也要能回到 LayoutRoot 路径。
        let address = base
            .layout_root
            .as_ref()
            .expect("已设置布局根")
            .container_children()
            .expect("包装器暴露子项")[0]
            .as_ref() as *const dyn Component as *const () as usize;
        assert_eq!(base.focus_target_of_address(address), Some(target));

        // 布局根本身（空路径）也可以聚焦。
        let root_target = FocusTarget::LayoutRoot {
            path: FocusPath::root(),
        };
        base.set_focus(Some(root_target));
        assert_eq!(base.focused_component(), Some(root_target));
    }

    /// Review 报告（minor）：TS 的 overlay handle 直接调用 `setFocus`，组件的 `focused` 属性
    /// 在 `hide()`/`unfocus()`/`setHidden(false)` 返回时已经是新值；Rust 的 handle 只写共享
    /// 状态，必须在下一次宿主入口（输入/渲染）刷新标志。这里验证渲染与输入入口都会刷新。
    #[test]
    fn render_and_input_entries_flush_focus_flags_from_overlay_handles() {
        let (probe, focused, inputs) = FocusProbe::new();
        let mut base = TuiBase::new(Box::new(StubTerminal::new(80, 24)), None, None);
        base.add_child(Box::new(probe));
        base.set_focus(Some(FocusTarget::Component { index: 0 }));
        assert!(focused.get());

        let (overlay_probe, overlay_focused, _) = FocusProbe::new();
        let mut handle = base.show_overlay(Box::new(overlay_probe), OverlayOptions::default());
        assert!(overlay_focused.get(), "捕获型 overlay 获得焦点");
        assert!(!focused.get(), "基础组件失去焦点");

        handle.hide();
        // hide() 恢复焦点到基础组件；渲染入口必须把标志同步过来（TS 里 hide() 内同步完成）。
        base.composite_overlays(Vec::new(), 80, 24);
        assert!(focused.get(), "渲染入口应刷新恢复后的 focused 标志");

        let (overlay_probe, overlay_focused, _) = FocusProbe::new();
        let mut handle = base.show_overlay(Box::new(overlay_probe), OverlayOptions::default());
        assert!(overlay_focused.get());
        handle.hide();
        base.handle_terminal_input("y");
        assert!(focused.get(), "输入入口应刷新恢复后的 focused 标志");
        assert_eq!(*inputs.borrow(), vec!["y".to_string()]);
    }

    /// Review 报告（minor）：TS 的 `setTimeout` 在 `queryTerminalColors` 调用时就创建，
    /// 超时从**调用**时刻算起；此前 Rust 的 `tokio::time::timeout` 在 future 第一次被 poll
    /// 时才启动，晚 await 会把截止后才到达的回复当成正常结果。
    #[tokio::test]
    async fn color_query_timeout_counts_from_the_call_not_from_the_await() {
        let mut base = TuiBase::new(Box::new(StubTerminal::new(80, 24)), None, None);
        let late: Rc<RefCell<Vec<TerminalColors>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&late);
        let query = base.query_terminal_colors_with_late_reply(
            20,
            Some(Box::new(move |colors| sink.borrow_mut().push(colors))),
        );

        // 截止时刻之后才到达回复，而且此前从未 poll 过 future。
        std::thread::sleep(std::time::Duration::from_millis(60));
        base.handle_terminal_input("\x1b]11;#ffffff\x07");

        // TS 的 promise 在 20ms 时已用**空**结果 resolve；白色是"迟到回复"。
        assert_eq!(query.await, TerminalColors::default());

        base.handle_terminal_input("\x1b[?62;22c");
        assert_eq!(
            *late.borrow(),
            vec![TerminalColors {
                foreground: None,
                background: Some(RgbColor {
                    r: 255,
                    g: 255,
                    b: 255
                }),
                palette: None,
            }],
            "迟到回复应在 DA1 时交给 onLateReply"
        );
    }

    /// 同一修复的边界：截止后先到一条颜色回复（同步补做 TS 计时器回调、记下截止时刻快照），
    /// 随后 DA1 把查询项移出队列，**之后**才 await —— TS 的 promise 在截止时刻已经以截止前
    /// 的部分结果 resolve，不受查询项被移出的影响。
    #[tokio::test]
    async fn timed_out_color_query_keeps_its_deadline_snapshot_after_da1_removes_it() {
        let mut base = TuiBase::new(Box::new(StubTerminal::new(80, 24)), None, None);
        let query = base.query_terminal_colors_with_late_reply(20, None);

        // 截止前收到背景色（黑色）。
        base.handle_terminal_input("\x1b]11;#000000\x07");
        std::thread::sleep(std::time::Duration::from_millis(60));
        // 截止后的迟到回复（不记录），DA1 紧接着移出查询项。
        base.handle_terminal_input("\x1b]10;#ffffff\x07");
        base.handle_terminal_input("\x1b[?62;22c");

        assert_eq!(
            query.await,
            TerminalColors {
                foreground: None,
                background: Some(RgbColor { r: 0, g: 0, b: 0 }),
                palette: None,
            },
            "promise 必须以截止时刻（而非 DA1 时）的部分结果 resolve"
        );
    }
}
