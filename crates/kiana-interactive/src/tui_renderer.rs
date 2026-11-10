//! modes/interactive/tui-renderer.ts。
//!
//! 交互模式的渲染器组合根：按 `tuiMode` 创建主屏 / 全屏渲染器，并提供一个可被宿主
//! 替换底层渲染器的稳定代理（TS `createInteractiveTuiReference`）。
//!
//! ## 与 TS 的表示差异
//!
//! * TS 的 `TUI` 是接口，`InteractiveMode` 用 `Proxy` 在运行时切换 `TuiMainScreen` /
//!   `TuiAltScreen`。Rust 没有动态属性读写：代理是 [`InteractiveTuiReference`]，方法始终
//!   转发到**当前**渲染器（[`InteractiveRenderer`] 枚举）。因此：
//!   - 不存在 `TUI property X is not callable` 这条错误路径（代理是静态分派，没有
//!     `Reflect.get` 拿到非函数的可能）。
//!   - TS 测试里 `tui.render = fn`（Proxy 的 `set` 陷阱）在 Rust 没有对应物；本模块用
//!     [`InteractiveTuiReference::capture_render`] / [`InteractiveTuiReference::capture_request_render`]
//!     暴露"取出的方法"：句柄每次调用都重新解析当前渲染器，因此模式切换后仍指向
//!     新渲染器，不会被替换前的实例钉住。
//! * [`Tui::children`] 需要返回底层渲染器的 `&mut Vec<Box<dyn Component>>`，代理无法穿过
//!   `Rc<RefCell<…>>` 借出它。交互模式真正需要的只是"取出全部子项"（`switchTuiMode` 的
//!   `[...previousUi.children]`），因此代理的 `children()` 会 panic（文档化的不可用路径），
//!   请改用 [`InteractiveTuiReference::take_children`]。
//! * 全屏专属方法（`getScreenLines`/`resetTextSelection`/`copyActiveSelectionToClipboard`…）
//!   在主屏上返回空值/`false`/无操作；TS 里这些方法只存在于 `TuiAltScreen`，通过 Proxy
//!   访问主屏渲染器会抛 TypeError，但交互模式只在 `instanceof TuiAltScreen` 分支调用。
//!
//! ## 本文件的组成
//!
//! - [`InteractiveTuiOptions`] + [`create_interactive_tui`]：组合根。按 `tui_mode` 二选一
//!   构造 [`TuiMainScreen`] / [`TuiAltScreen`]，并把全屏专属的搜索高亮、跳到底部提示、
//!   打开链接、复制选择等回调注入 [`TuiAltScreenOptions`]；返回
//!   `Rc<RefCell<InteractiveRenderer>>` 供后续共享。
//! - [`InteractiveRenderer`]：主屏/全屏的静态分派。所有方法按当前枚举变体转发；主屏没有
//!   的全屏能力按"空值 / `false` / 无操作"降级（见上）。
//! - [`InteractiveTuiReference`]：给调用方的稳定句柄。模式切换只替换 `RefCell` 里的枚举
//!   值，句柄以及从它取出的 `capture_*` 闭包始终指向当前渲染器。
//! - [`SharedComponentProxy`] / [`SharedLoader`] / [`EditorSlot`]：把"容器里的组件"与
//!   "宿主/扩展持有的共享句柄"桥接起来的代理与编辑器槽位。
//!
//! ## 调用方与生命周期
//!
//! - `interactive_mode` 的初始化与 `switch_tui_mode` 调用 [`create_interactive_tui`] /
//!   [`create_interactive_tui_reference`]：先构造渲染器，再包一层引用交给交互模式与扩展 UI。
//! - `InteractiveMode::run` 启动的轮询任务每 10 ms 调一次
//!   [`InteractiveTuiReference::pump_events`]：轮询终端输入/定时器/尺寸 → 分发输入 →
//!   到点渲染一帧。常规的输入分发与逐帧渲染由这条循环驱动；启动、退出与测试等路径也会
//!   直接调用 `render_now`，但它们同样运行在同一个 `LocalSet` 线程上。
//! - 模式切换是"取出再交还"：调用方先用 [`InteractiveRenderer::focused_component_identity`]
//!   保存焦点地址，再 [`InteractiveRenderer::take_children`] 取走全部子项，`stop` 旧渲染器，
//!   替换枚举值后重新挂载。因此这些方法在本文件里成对出现。
//!
//! ## 借用约束与失败模式
//!
//! - 句柄全部是 `Rc<RefCell<…>>`（非 `Send`/`Sync`）；`copy_selection_to_clipboard` 这条
//!   路径使用 `tokio::task::spawn_local`，要求运行在单线程 `LocalSet` 上。
//! - 普通方法直接 `borrow()`/`borrow_mut()`：在已持有同一 `RefCell` 借用时调用会 panic。
//!   可能重入的入口（[`InteractiveTuiReference::request_render`]、
//!   [`focus_component_address`]、[`request_render_callback`]）改用 `try_borrow_mut()`，
//!   借不到时静默跳过，依赖调用方后续的渲染请求补上这一帧。
//! - 本文件的 `fg`/`bg`/`bold`/`inverse`/`underline` 辅助函数在主题未初始化或 token
//!   缺失时 panic（对应 TS 的抛错）。它们只在渲染期的样式闭包里被调用，因此构造渲染器
//!   本身不读取主题。
//!
//! ## 补充：为什么是“枚举 + 句柄”两层
//!
//! [`Tui`] 只声明主屏 / 全屏共有的能力；`capture_render_state`、`set_layout_root`、
//! `get_screen_lines`、`copy_active_selection_to_clipboard` 等只有单一变体才有的方法不在
//! trait 里。保留 [`InteractiveRenderer`] 枚举，就能在 `match` 的变体分支里直接调用这些
//! 具体方法，并在另一侧给出明确的降级值（`None` / 空列表 / `false` / 无操作）。
//! [`InteractiveTuiReference`] 只保存 `Rc<RefCell<InteractiveRenderer>>`，所以模式切换
//! 只需替换 `RefCell` 的内容，已经发出去的句柄不必重新绑定。
//!
//! ## 补充：修改时的影响面
//!
//! * [`InteractiveRenderer::take_children`] 直接取走 `children` 向量，不走
//!   `remove_child_at`，因此不会刷新子项 id、鼠标布局与脱离保活列表；调用方
//!   `interactive_mode::switch_tui_mode` 只在即将 `stop` 并迁移组件时使用它。
//! * `switch_tui_mode` 按固定顺序调用 [`InteractiveRenderer::focused_component_identity`]
//!   → [`InteractiveRenderer::take_children`] → `stop` → 替换枚举值 → `set_focus`；调整
//!   其中任何一步都会破坏焦点在新旧渲染器之间的迁移。
//! * 在 [`InteractiveRenderer`] 上新增能力时，通常也要在 [`InteractiveTuiReference`]
//!   添一个同名包装，否则只拿到句柄的调用方看不到它。
//! * [`SharedComponentProxy`] / [`EditorSlot`] / [`SharedLoader`] 转发时直接
//!   `borrow()`/`borrow_mut()` 共享句柄。若在组件渲染 / 输入回调内部再次借用同一句柄，
//!   `RefCell` 会 panic；可重入入口（[`InteractiveTuiReference::request_render`]、
//!   [`focus_component_address`]、[`request_render_callback`]）已改用 `try_borrow_mut()`，
//!   借用失败时静默跳过（`SharedComponentProxy::as_focusable` 的探测同样如此）。
//!
//! ## 阅读顺序
//!
//! [`InteractiveTuiOptions`] → [`create_interactive_tui`] → [`InteractiveRenderer`] 的分派 →
//! [`InteractiveTuiReference`] → 共享代理（[`SharedComponentProxy`]、[`EditorSlot`]、
//! [`SharedLoader`]）。

use std::cell::{Cell, RefCell};
use std::pin::Pin;
use std::rc::Rc;

use kiana_core::clipboard::{copy_to_clipboard, open_browser};
use kiana_theme::theme_tokens::{ThemeBg, ThemeColor};
use kiana_tui::alt_screen::{
    CopySelectionDispatch, CopySelectionDone, CopySelectionResult, OpenUrlFn, RightClickPasteFn,
    ScrollToEndIndicatorFn, SearchNavigationButtonStyle, TuiAltScreen, TuiAltScreenOptions,
};
use kiana_tui::components::loader::RequestRenderCallback;
use kiana_tui::keybindings::Keybinding;
use kiana_tui::layout_node::SharedComponent;
use kiana_tui::main_screen::{TuiMainScreen, TuiMainScreenRenderState};
use kiana_tui::terminal::ProcessTerminal;
use kiana_tui::terminal_colors::{Terminal, TerminalColors};
use kiana_tui::tui::{
    Component, Container, FocusTarget, Focusable, InputListenerId, OverlayHandle, OverlayOptions,
    Tui, TuiInputListener, TuiMode, TuiMouseEvent, TuiMouseEventResult, TuiStopOptions,
    ViewportTui,
};
use kiana_tui::wheel::WheelScrollLines;
use kiana_ui_kit::keybinding_hints::key_display_text;

/// `ALT_SCREEN_BOTTOM`：跳到底部提示引用的快捷键（`tui.altScreen.bottom`）。
///
/// 全屏渲染器的"跳到底部"指示条（[`create_interactive_tui`] 里的
/// `scroll_to_end_indicator`）用它把快捷键名解析成用户可读文本（如 `Ctrl+End`）；
/// 快捷键表里没有该绑定或显示文本为空时，指示条只显示文案、不带快捷键。
const ALT_SCREEN_BOTTOM: Keybinding = "tui.altScreen.bottom";

// 这组函数包装 [`kiana_theme::THEME`]（全局主题代理），把 `Result` 折叠成 panic：
// - 主题未初始化：代理返回 `Theme not initialized. Call initTheme() first.`；
// - token 缺失：`fg`/`bg` 在当前主题的颜色表里找不到 token 时返回
//   `Unknown theme color: …`。
// 两者在这里都 `expect` 成 panic（对应 TS 版本在同样条件下直接 throw）。调用点只有
// [`create_interactive_tui`] 注入的全屏样式闭包，即这些 panic 只发生在渲染期的样式
// 计算里——构造渲染器、切换模式等路径不读主题，不会触发。
fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

fn bg(color: ThemeBg, text: &str) -> String {
    kiana_theme::THEME
        .bg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

fn bold(text: &str) -> String {
    kiana_theme::THEME
        .bold(text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

fn inverse(text: &str) -> String {
    kiana_theme::THEME
        .inverse(text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

fn underline(text: &str) -> String {
    kiana_theme::THEME
        .underline(text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// `InteractiveTuiOptions`（`createInteractiveTui` 的入参）。
///
/// `terminal` 的所有权移入渲染器；不传时新建 `ProcessTerminal`。可选项为 `None` 时由
/// 下层渲染器决定默认值：全屏的 `copy_on_select` 默认 `true`，`wheel_scroll_lines` 在
/// [`create_interactive_tui`] 里固定落为 `WheelScrollLines::Auto`。
pub struct InteractiveTuiOptions {
    /// TS `tuiMode: "regular" | "fullscreen"`。
    pub tui_mode: TuiMode,
    /// TS `showHardwareCursor`。
    pub show_hardware_cursor: bool,
    /// TS `logDirectory`（崩溃/调试日志目录）。
    pub log_directory: String,
    /// TS `terminal`；缺省 `new ProcessTerminal()`。
    pub terminal: Option<Box<dyn Terminal>>,
    /// TS `onRightClickPaste`。
    pub on_right_click_paste: Option<RightClickPasteFn>,
    /// TS `fullscreenCopyOnSelect`。
    pub fullscreen_copy_on_select: Option<bool>,
    /// TS `fullscreenWheelScrollLines`。
    pub fullscreen_wheel_scroll_lines: Option<WheelScrollLines>,
}

/// 主屏 / 全屏渲染器的静态分派（TS 运行时用 `instanceof` 区分两者）。
///
/// 每个公开方法都按当前变体转发；只有一侧存在的能力按“空值 / `false` / 无操作”降级，
/// 具体见各方法文档。调用方不应假定变体在整个生命周期内不变：`switch_tui_mode` 会原地
/// 替换 `RefCell` 里的值。
pub enum InteractiveRenderer {
    /// `tuiMode === "regular"`。
    Regular(TuiMainScreen),
    /// `tuiMode === "fullscreen"`。
    Fullscreen(TuiAltScreen),
}

impl InteractiveRenderer {
    /// 当前渲染器（[`Tui`] 视图）。
    fn as_tui(&mut self) -> &mut dyn Tui {
        match self {
            InteractiveRenderer::Regular(renderer) => renderer,
            InteractiveRenderer::Fullscreen(renderer) => renderer,
        }
    }

    /// 当前渲染器（只读 [`Tui`] 视图）。
    fn as_tui_ref(&self) -> &dyn Tui {
        match self {
            InteractiveRenderer::Regular(renderer) => renderer,
            InteractiveRenderer::Fullscreen(renderer) => renderer,
        }
    }

    /// 底层 [`TuiBase`](kiana_tui::tui::TuiBase)（overlay/焦点/调试状态）。
    fn base(&self) -> &kiana_tui::tui::TuiBase {
        match self {
            InteractiveRenderer::Regular(renderer) => renderer.base(),
            InteractiveRenderer::Fullscreen(renderer) => renderer.base(),
        }
    }

    /// 底层 [`TuiBase`](kiana_tui::tui::TuiBase)（可变）。
    fn base_mut(&mut self) -> &mut kiana_tui::tui::TuiBase {
        match self {
            InteractiveRenderer::Regular(renderer) => renderer.base_mut(),
            InteractiveRenderer::Fullscreen(renderer) => renderer.base_mut(),
        }
    }

    /// `mode`。
    pub fn mode(&self) -> TuiMode {
        self.as_tui_ref().mode()
    }

    /// `terminalColorSchemeNotifications` 的当前值（主题 auto-sync 的可观察状态）。
    pub fn terminal_color_scheme_notifications_enabled(&self) -> bool {
        self.base().terminal_color_scheme_notifications_enabled
    }

    /// `isViewportTUI(tui)`。
    pub fn is_viewport_tui(&self) -> bool {
        self.as_tui_ref().is_viewport_tui()
    }

    /// `addChild(component)`。
    pub fn add_child(&mut self, child: Box<dyn Component>) {
        self.as_tui().add_child(child);
    }

    /// `removeChild(component)`。
    pub fn remove_child(&mut self, child: &dyn Component) {
        self.as_tui().remove_child(child);
    }

    /// 按下标移除子项。
    pub fn remove_child_at(&mut self, index: usize) -> Option<Box<dyn Component>> {
        self.as_tui().remove_child_at(index)
    }

    /// `clear()`：清空顶层子项。
    ///
    /// 主屏实现是逐个 `remove_child_at(0)`，遇到持焦子项返回 `None` 就提前停止，剩余子项
    /// 仍留在 `children` 里；因此“清空”不保证完成。`switch_tui_mode` 先 `set_focus(None)`
    /// 再调用它来规避这一点。
    pub fn clear(&mut self) {
        self.as_tui().clear_children();
    }

    /// `[...children]`（TS `switchTuiMode`）：取出全部顶层子项。
    ///
    /// TS 读取 `previousUi.children` 后 `clear()`；Rust 用 `take` 一步完成，语义相同
    /// （调用后渲染器不再持有这些子项）。
    ///
    /// 注意：直接替换底层 `children` 向量，不经过 `remove_child_at`，因此不会更新子项 id、
    /// 鼠标布局与脱离保活列表。只在“即将停止旧渲染器并迁移组件”的路径使用
    /// （`switch_tui_mode`），不要拿它当常规删除接口。
    pub fn take_children(&mut self) -> Vec<Box<dyn Component>> {
        std::mem::take(self.as_tui().children())
    }

    /// `children.length`。
    pub fn children_len(&mut self) -> usize {
        self.as_tui().children().len()
    }

    /// 在渲染器内部借用期间访问顶层子项（供 `TuiArcAdapter` 覆写
    /// [`kiana_tui::tui::Tui::with_children`]）。
    pub fn with_children(&mut self, f: &mut dyn FnMut(&mut Vec<Box<dyn Component>>)) {
        f(self.as_tui().children())
    }

    /// `setLayoutRoot(component)`（仅全屏渲染器；主屏无操作，与 TS `isViewportTUI` 判定一致）。
    pub fn set_layout_root(&mut self, component: Option<Box<dyn Component>>) {
        if let InteractiveRenderer::Fullscreen(renderer) = self {
            renderer.set_layout_root(component);
        }
    }

    /// `getFocusedComponent()`（以焦点目标表示）。
    pub fn focus(&self) -> Option<FocusTarget> {
        self.as_tui_ref().focus()
    }

    /// `setFocus(target)`。
    pub fn set_focus(&mut self, target: Option<FocusTarget>) {
        self.as_tui().set_focus(target);
    }

    /// `start()`。
    pub fn start(&mut self) {
        self.as_tui().start();
    }

    /// 宿主循环一步：轮询 stdin、定时器与尺寸，分发排队输入，并在到点时渲染一帧。
    ///
    /// TS 的 Node 事件循环把这些拆成 `stdin.on("data")`、`setTimeout` 与
    /// `stdout.on("resize")` 回调；Rust 用一个由 `InteractiveMode::run` 驱动的轮询任务
    /// 复刻。轮询顺序与 `TuiBase::start` 的注释一致：先喂输入（推进 `pending_input`），
    /// 再分发（`handleTerminalInput`），最后按 `renderDecision` 渲染。
    ///
    /// 具体阶段：
    /// 1. 在同一把终端借用内依次 `poll_input` → `poll_timers` → `poll_resize`；
    /// 2. 把排队的原始输入交给组件：主屏走 `TuiBase::pump_pending_input`，全屏走
    ///    `TuiAltScreen::pump_pending_input`（其中包含视口输入与搜索查询更新）；
    /// 3. [`InteractiveRenderer::flush_pending_render`]，只有渲染决策为 `Now` 时才真正
    ///    输出一帧。
    ///
    /// 第 2 步会调用组件回调，回调可能间接借用同一渲染器；调用方必须在不持有其它借用时
    /// 调用本方法。
    pub fn pump_events(&mut self) {
        let terminal = self.terminal();
        {
            let mut terminal = terminal.borrow_mut();
            terminal.poll_input();
            terminal.poll_timers(std::time::Instant::now());
            terminal.poll_resize();
        }
        match self {
            InteractiveRenderer::Regular(renderer) => {
                renderer.base_mut().pump_pending_input();
            }
            InteractiveRenderer::Fullscreen(renderer) => renderer.pump_pending_input(),
        }
        self.flush_pending_render();
    }

    /// 宿主渲染决策为 `Now` 时渲染一帧并返回 `true`（`flushPendingRender`）。
    ///
    /// 主屏的 [`TuiMainScreen::flush_pending_render`] 内部就包含“取决策 + 渲染”；全屏需要
    /// 先调用 `TuiAltScreen::take_render_decision_now`（它还会检查 flash / 定时器截止时间），
    /// 再 `render_now(false)`。返回 `false` 表示本步没有渲染（没有请求、节流窗口未到或已停止）。
    pub fn flush_pending_render(&mut self) -> bool {
        match self {
            InteractiveRenderer::Regular(renderer) => renderer.flush_pending_render(),
            InteractiveRenderer::Fullscreen(renderer) => {
                if renderer.take_render_decision_now() {
                    renderer.render_now(false);
                    true
                } else {
                    false
                }
            }
        }
    }

    /// `stop(options)`。
    pub fn stop(&mut self, options: TuiStopOptions) {
        self.as_tui().stop(options);
    }

    /// `renderNow(force)`。
    pub fn render_now(&mut self, force: bool) {
        self.as_tui().render_now(force);
    }

    /// `requestRender(force)`。
    pub fn request_render(&mut self, force: bool) {
        self.as_tui().request_render(force);
    }

    /// `invalidate()`。
    pub fn invalidate(&mut self) {
        self.as_tui().invalidate();
    }

    /// `render(width)`。
    pub fn render(&mut self, width: usize) -> Vec<String> {
        self.as_tui().render(width)
    }

    /// `handleInput(data)`。
    pub fn handle_input(&mut self, data: &str) {
        self.as_tui().handle_input(data);
    }

    /// 终端输入入口：全屏渲染器必须先经过 alt screen 的视口处理（鼠标选择、搜索等），
    /// 对应 `TuiAltScreen::handle_terminal_input`。
    ///
    /// 主屏直接调用 `handle_input`（没有视口层）；全屏的视口处理会先于组件键盘分发执行，
    /// 并在分发后同步焦点标志、更新待处理的搜索查询。逐帧驱动由
    /// [`InteractiveRenderer::pump_events`] 完成，本方法只处理一条已解码的输入。
    pub fn handle_terminal_input(&mut self, data: &str) {
        match self {
            InteractiveRenderer::Regular(renderer) => renderer.handle_input(data),
            InteractiveRenderer::Fullscreen(renderer) => renderer.handle_terminal_input(data),
        }
    }

    /// `handleMouse(event)`。
    pub fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.as_tui().handle_mouse(event)
    }

    /// `terminal`。
    ///
    /// 返回底层终端的共享句柄（与渲染器共用同一个 `Rc<RefCell<…>>`）；克隆它不会复制
    /// 终端，`switch_tui_mode` 正是靠这一点让新旧渲染器复用同一个终端。
    pub fn terminal(&self) -> Rc<RefCell<Box<dyn Terminal>>> {
        match self {
            InteractiveRenderer::Regular(renderer) => Rc::clone(renderer.terminal()),
            InteractiveRenderer::Fullscreen(renderer) => Rc::clone(renderer.terminal()),
        }
    }

    /// `getShowHardwareCursor()`。
    pub fn get_show_hardware_cursor(&self) -> bool {
        match self {
            InteractiveRenderer::Regular(renderer) => renderer.get_show_hardware_cursor(),
            InteractiveRenderer::Fullscreen(renderer) => renderer.get_show_hardware_cursor(),
        }
    }

    /// `setShowHardwareCursor(enabled)`。
    pub fn set_show_hardware_cursor(&mut self, enabled: bool) {
        match self {
            InteractiveRenderer::Regular(renderer) => renderer.set_show_hardware_cursor(enabled),
            InteractiveRenderer::Fullscreen(renderer) => renderer.set_show_hardware_cursor(enabled),
        }
    }

    /// `getClearOnShrink()`。
    pub fn get_clear_on_shrink(&self) -> bool {
        match self {
            InteractiveRenderer::Regular(renderer) => renderer.get_clear_on_shrink(),
            InteractiveRenderer::Fullscreen(renderer) => renderer.get_clear_on_shrink(),
        }
    }

    /// `setClearOnShrink(enabled)`。
    pub fn set_clear_on_shrink(&mut self, enabled: bool) {
        match self {
            InteractiveRenderer::Regular(renderer) => renderer.set_clear_on_shrink(enabled),
            InteractiveRenderer::Fullscreen(renderer) => renderer.set_clear_on_shrink(enabled),
        }
    }

    /// `captureRenderState()`（仅主屏；全屏渲染器没有该成员，返回 `None`）。
    pub fn capture_render_state(&self) -> Option<TuiMainScreenRenderState> {
        match self {
            InteractiveRenderer::Regular(renderer) => Some(renderer.capture_render_state()),
            InteractiveRenderer::Fullscreen(_) => None,
        }
    }

    /// `restoreRenderState(state)`（仅主屏；全屏渲染器无操作）。
    pub fn restore_render_state(&mut self, state: TuiMainScreenRenderState) {
        if let InteractiveRenderer::Regular(renderer) = self {
            renderer.restore_render_state(state);
        }
    }

    /// `setCopyOnSelect(enabled)`（仅全屏）。
    pub fn set_copy_on_select(&mut self, enabled: bool) {
        if let InteractiveRenderer::Fullscreen(renderer) = self {
            renderer.set_copy_on_select(enabled);
        }
    }

    /// `getCopyOnSelect()`（仅全屏；主屏返回 `false`，TS 里只有全屏分支会调用）。
    pub fn get_copy_on_select(&self) -> bool {
        match self {
            InteractiveRenderer::Regular(_) => false,
            InteractiveRenderer::Fullscreen(renderer) => renderer.get_copy_on_select(),
        }
    }

    /// `setWheelScrollLines(lines)`（仅全屏）。
    pub fn set_wheel_scroll_lines(&mut self, lines: WheelScrollLines) {
        if let InteractiveRenderer::Fullscreen(renderer) = self {
            renderer.set_wheel_scroll_lines(lines);
        }
    }

    /// 取出 `onDebug` 回调（TS `previousUi.onDebug`）。
    ///
    /// Rust 的 `Box<dyn FnMut()>` 不能克隆，取值即移出；调用方负责在切换后
    /// [`InteractiveRenderer::set_on_debug`] 交还给新渲染器。
    pub fn take_on_debug(&mut self) -> Option<Box<dyn FnMut()>> {
        self.base_mut().on_debug.take()
    }

    /// `nextUi.onDebug = callback`。
    pub fn set_on_debug(&mut self, callback: Option<Box<dyn FnMut()>>) {
        self.base_mut().on_debug = callback;
    }

    /// `hasOverlayEntries`（含隐藏项）。
    pub fn has_overlay_entries(&self) -> bool {
        self.base().has_overlay_entries()
    }

    /// `hasOverlay()`。
    pub fn has_overlay(&self) -> bool {
        self.as_tui_ref().has_overlay()
    }

    /// `hideOverlay()`。
    pub fn hide_overlay(&mut self) {
        self.as_tui().hide_overlay();
    }

    /// `showOverlay(component, options)`。
    pub fn show_overlay(
        &mut self,
        component: Box<dyn Component>,
        options: OverlayOptions,
    ) -> Box<dyn OverlayHandle> {
        self.as_tui().show_overlay(component, options)
    }

    /// `flash(message, durationMs?)`（仅全屏；主屏无操作）。
    pub fn flash(&mut self, message: &str, duration_ms: Option<f64>) {
        if let InteractiveRenderer::Fullscreen(renderer) = self {
            renderer.flash(message, duration_ms);
        }
    }

    /// `getScreenLines()`（仅全屏；主屏没有该方法，返回空列表）。
    pub fn get_screen_lines(&self) -> Vec<String> {
        match self {
            InteractiveRenderer::Regular(_) => Vec::new(),
            InteractiveRenderer::Fullscreen(renderer) => renderer.get_screen_lines(),
        }
    }

    /// `resetTextSelection()`（仅全屏；主屏无操作）。
    pub fn reset_text_selection(&mut self) {
        if let InteractiveRenderer::Fullscreen(renderer) = self {
            renderer.reset_text_selection();
        }
    }

    /// `hasActiveSelection()`（仅全屏；主屏返回 `false`）。
    pub fn has_active_selection(&self) -> bool {
        match self {
            InteractiveRenderer::Regular(_) => false,
            InteractiveRenderer::Fullscreen(renderer) => renderer.has_active_selection(),
        }
    }

    /// `copyActiveSelectionToClipboard()`（仅全屏；主屏返回 `Complete(false)`）。
    pub fn copy_active_selection_to_clipboard(
        &mut self,
    ) -> kiana_tui::alt_screen::CopySelectionStatus {
        match self {
            InteractiveRenderer::Regular(_) => {
                kiana_tui::alt_screen::CopySelectionStatus::Complete(false)
            }
            InteractiveRenderer::Fullscreen(renderer) => {
                renderer.copy_active_selection_to_clipboard()
            }
        }
    }

    /// `renderRequested` 的共享句柄（宿主渲染循环的调度标志）。
    pub fn render_requested_handle(&self) -> Rc<Cell<bool>> {
        match self {
            InteractiveRenderer::Regular(renderer) => renderer.base().render_requested_handle(),
            InteractiveRenderer::Fullscreen(renderer) => renderer.render_requested_handle(),
        }
    }

    /// `addInputListener(listener)`。
    pub fn add_input_listener(&mut self, listener: TuiInputListener) -> InputListenerId {
        self.as_tui().add_input_listener(listener)
    }

    /// `removeInputListener(id)`。
    pub fn remove_input_listener(&mut self, id: InputListenerId) {
        self.as_tui().remove_input_listener(id);
    }

    /// 组件地址 → 焦点目标（[`FocusTarget`]）。
    ///
    /// TS 的 `setFocus(component)` 按引用在挂载树里查找；Rust 的焦点目标是位置，
    /// 因此先用组件地址在挂载树里定位（未挂载时返回 `None`，与"聚焦一个不在树上的
    /// 组件"一样无法把输入路由过去）。
    ///
    /// 查找顺序：先布局根（全屏 `set_layout_root` 之后挂载根就是它），再按顺序遍历顶层
    /// `children`。命中顶层子项本身时得到 [`FocusTarget::Component`]，更深层得到 `Path` /
    /// `LayoutRoot` 路径。
    pub fn focus_target_of_address(&self, address: usize) -> Option<FocusTarget> {
        self.base().focus_target_of_address(address)
    }

    /// `getFocusedComponent()`（TS 的组件引用）：解析当前焦点目标对应的组件身份。
    ///
    /// TS 的 `getFocusedComponent()` 返回组件引用；Rust 的 [`FocusTarget`] 是挂载树里的
    /// 位置，因此额外解析出组件地址（`&dyn Component` 的数据指针）作为"引用相等"的
    /// 等价物。焦点指向已脱离挂载树的组件（[`TuiBase::focused_component`](kiana_tui::tui::TuiBase::focused_component) 返回 `None`）
    /// 时同样返回 `None`。
    ///
    /// `address` 是 `&dyn Component` 的数据指针：`Box` 移动不改变它，所以可以先在旧渲染器
    /// 上取值、迁移组件后在新渲染器里按地址重新定焦（`switch_tui_mode` 的做法）；但组件
    /// 释放后地址可能被新分配复用，不能长期保存。
    pub fn focused_component_identity(&self) -> Option<FocusedComponentIdentity> {
        let target = self.focus()?;
        let base = self.base();
        match target {
            FocusTarget::Overlay { id } => {
                let core = base.core().borrow();
                let entry = core.overlay_stack().iter().find(|entry| entry.id == id)?;
                Some(FocusedComponentIdentity {
                    target,
                    address: component_address(entry.component.as_ref()),
                    handles_input: entry.component.handles_input(),
                })
            }
            FocusTarget::Component { index } => {
                let child = base.children.get(index)?;
                Some(FocusedComponentIdentity {
                    target,
                    address: component_address(child.as_ref()),
                    handles_input: child.handles_input(),
                })
            }
            FocusTarget::Path { path } => {
                let indices = path.indices();
                let (first, rest) = indices.split_first()?;
                let root = base.children.get(*first as usize)?;
                let (address, handles_input) = focused_path_info(root.as_ref(), rest)?;
                Some(FocusedComponentIdentity {
                    target,
                    address,
                    handles_input,
                })
            }
            FocusTarget::LayoutRoot { path } => {
                let root = base.layout_root.as_deref()?;
                let (address, handles_input) = focused_path_info(root, path.indices())?;
                Some(FocusedComponentIdentity {
                    target,
                    address,
                    handles_input,
                })
            }
        }
    }

    /// `focusedComponent.handleInput(data)`：把输入直接交给当前焦点组件。
    ///
    /// 与 TS 一样**绕过**键盘分发（监听器、键位匹配），用于右键粘贴等需要直接投递
    /// 组件输入的路径。返回是否实际投递（无焦点 / 目标没有 `handleInput` / 目标已脱离
    /// 挂载树时为 `false`）。
    ///
    /// 路径里的下标是 `for_each_container_child` 的遍历序号（与容器子项顺序一致，能穿过
    /// 共享代理）；越界即返回 `false`。本方法只投递输入，不请求重绘。
    pub fn dispatch_input_to_focused_component(&mut self, data: &str) -> bool {
        let Some(target) = self.focus() else {
            return false;
        };
        match target {
            FocusTarget::Overlay { id } => {
                let core = self.base().core();
                let mut core = core.borrow_mut();
                let Some(component) = core.overlay_component_mut(id) else {
                    return false;
                };
                dispatch_component_input(component, data)
            }
            FocusTarget::Component { index } => {
                let Some(child) = self.base_mut().children.get_mut(index) else {
                    return false;
                };
                dispatch_component_input(child.as_mut(), data)
            }
            FocusTarget::Path { path } => {
                let indices = path.indices();
                let Some((first, rest)) = indices.split_first() else {
                    return false;
                };
                let Some(root) = self.base_mut().children.get_mut(*first as usize) else {
                    return false;
                };
                dispatch_input_at_path(root.as_mut(), rest, data)
            }
            FocusTarget::LayoutRoot { path } => {
                let Some(root) = self.base_mut().layout_root.as_mut() else {
                    return false;
                };
                dispatch_input_at_path(root.as_mut(), path.indices(), data)
            }
        }
    }
}

/// [`InteractiveRenderer::focused_component_identity`] 的结果。
///
/// `target`/`address` 一起构成 TS `getFocusedComponent()` 的引用身份（位置 + 分配地址），
/// `handles_input` 对应 TS 的 `"handleInput" in target` 判定。
///
/// 三个字段都是一次查询的快照，不延长组件生命周期；`target` 是旧渲染器里的位置，不能
/// 直接交给新渲染器使用，跨渲染器迁移要用 `address` 重新解析。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusedComponentIdentity {
    /// 焦点目标（Rust 的"组件引用"）。
    pub target: FocusTarget,
    /// 组件地址（`&dyn Component` 的数据指针；分配不变即同一个组件）。
    ///
    /// 只保证“组件存活期间”有效；释放后同一地址可能被复用。
    pub address: usize,
    /// 组件是否有输入处理器（TS 的可选 `handleInput` 成员）。
    pub handles_input: bool,
}

/// 沿下标路径解析组件的 `(地址, 是否有 handleInput)`（空路径 = `component` 自己）。
///
/// 下标是 `for_each_container_child` 的遍历序号；越界或路径指向不存在的子项时返回
/// `None`。命中后遍历立即停止（回调返回 `false`）。
fn focused_path_info(component: &dyn Component, indices: &[u32]) -> Option<(usize, bool)> {
    let Some((first, rest)) = indices.split_first() else {
        return Some((component_address(component), component.handles_input()));
    };
    let index = usize::try_from(*first).ok()?;
    let mut found = None;
    let mut position = 0usize;
    component.for_each_container_child(&mut |child| {
        if Some(position) == Some(index) {
            found = focused_path_info(child, rest);
            false
        } else {
            position += 1;
            true
        }
    });
    found
}

/// 把 `data` 交给 `component`（TS `target.handleInput(data)`）。
///
/// `handles_input()` 为假时返回 `false` 且不调用 `handle_input`，否则投递并返回 `true`。
fn dispatch_component_input(component: &mut (dyn Component + 'static), data: &str) -> bool {
    if !component.handles_input() {
        return false;
    }
    component.handle_input(data);
    true
}

/// 沿下标路径把 `data` 交给末端组件（空路径 = `component` 自己）。
///
/// 每一层的下标对应 `for_each_container_child_mut` 的遍历序号；任一层越界或末端组件不
/// 处理输入时返回 `false`。命中后遍历立即停止。
fn dispatch_input_at_path(
    component: &mut (dyn Component + 'static),
    indices: &[u32],
    data: &str,
) -> bool {
    let Some((first, rest)) = indices.split_first() else {
        return dispatch_component_input(component, data);
    };
    let index = usize::try_from(*first).ok();
    let mut dispatched = false;
    let mut position = 0usize;
    component.for_each_container_child_mut(&mut |child| {
        if Some(position) == index {
            dispatched = dispatch_input_at_path(child, rest, data);
            false
        } else {
            position += 1;
            true
        }
    });
    dispatched
}

/// 执行 `copySelection(text)` 的异步剪贴板写入（TS `async (text) => …`）。
///
/// 用 `tokio::task::spawn_local` 执行：完成回调 `done` 是 `Rc<dyn Fn>`（非 `Send`），
/// 不能用普通 `spawn`；当前线程必须有运行中的 `LocalSet`。本函数立即返回
/// [`CopySelectionDispatch::Pending`]，写入结束后用 `Ok` / 错误文案调用一次 `done`。
fn copy_selection_to_clipboard(text: &str, done: CopySelectionDone) -> CopySelectionDispatch {
    let text = text.to_string();
    tokio::task::spawn_local(async move {
        let result = match copy_to_clipboard(&text).await {
            Ok(()) => CopySelectionResult::Ok,
            // TS: `error instanceof Error ? error.message : String(error)`。
            Err(error) => CopySelectionResult::Error(error.to_string()),
        };
        done(result);
    });
    CopySelectionDispatch::Pending
}

/// `createInteractiveTui(options)`。
///
/// 流程：取 `options.terminal`（缺省新建 `ProcessTerminal`）→ 按 `tui_mode` 构造
/// `TuiAltScreen` / `TuiMainScreen` → 包进 `Rc<RefCell<…>>` 返回，供
/// [`InteractiveTuiReference`] 与 `switch_tui_mode` 共享。
///
/// 全屏分支注入的样式回调都在**调用时**读取全局主题（`THEME`），不是构造时的快照，
/// 因此主题切换后不需要重建渲染器。搜索匹配样式由 `SearchMatchBg` / `SearchMatchText` /
/// `SelectedBg` 组合；跳到底部提示的快捷键文本来自
/// `key_display_text("tui.altScreen.bottom")`，取不到时只显示文案。`copy_selection` 走
/// 异步剪贴板（要求 `LocalSet`）。主屏分支只构造 `TuiMainScreen`，不注入这些回调。
pub fn create_interactive_tui(options: InteractiveTuiOptions) -> Rc<RefCell<InteractiveRenderer>> {
    let terminal: Box<dyn Terminal> = options
        .terminal
        .unwrap_or_else(|| Box::new(ProcessTerminal::new()));
    let renderer = match options.tui_mode {
        TuiMode::Fullscreen => {
            let style_search_match = |text: &str| {
                bg(
                    ThemeBg::SearchMatchBg,
                    &fg(ThemeColor::SearchMatchText, text),
                )
            };
            let search_match_style: kiana_tui::alt_screen::AltStyleFn =
                { Rc::new(move |text: &str| underline(&style_search_match(text))) };
            let search_current_match_style: kiana_tui::alt_screen::AltStyleFn =
                { Rc::new(move |text: &str| bold(&inverse(&style_search_match(text)))) };
            let search_navigation_button_style: SearchNavigationButtonStyle =
                Rc::new(|text: &str, hovered: bool| {
                    if hovered {
                        underline(text)
                    } else {
                        text.to_string()
                    }
                });
            let scroll_to_end_indicator: ScrollToEndIndicatorFn = Rc::new(|| {
                let shortcut = key_display_text(ALT_SCREEN_BOTTOM);
                let label = if shortcut.is_empty() {
                    " ↓ Jump to latest message ".to_string()
                } else {
                    format!(" ↓ Jump to latest message · {shortcut} ")
                };
                bg(ThemeBg::SelectedBg, &fg(ThemeColor::Text, &label))
            });
            let open_url: OpenUrlFn = Rc::new(|url: &str| open_browser(url));
            let copy_selection: kiana_tui::alt_screen::CopySelectionFn =
                Rc::new(copy_selection_to_clipboard);
            InteractiveRenderer::Fullscreen(TuiAltScreen::new(
                terminal,
                Some(options.show_hardware_cursor),
                Some(options.log_directory),
                TuiAltScreenOptions {
                    search_match_style: Some(search_match_style),
                    search_current_match_style: Some(search_current_match_style),
                    search_navigation_button_style: Some(search_navigation_button_style),
                    scroll_to_end_indicator: Some(scroll_to_end_indicator),
                    open_url: Some(open_url),
                    on_right_click_paste: options.on_right_click_paste,
                    copy_on_select: options.fullscreen_copy_on_select,
                    wheel_scroll_lines: Some(
                        options
                            .fullscreen_wheel_scroll_lines
                            .unwrap_or(WheelScrollLines::Auto),
                    ),
                    copy_selection: Some(copy_selection),
                    ..Default::default()
                },
            ))
        }
        TuiMode::Regular => InteractiveRenderer::Regular(TuiMainScreen::with_options(
            terminal,
            Some(options.show_hardware_cursor),
            Some(options.log_directory),
        )),
    };
    Rc::new(RefCell::new(renderer))
}

/// `createInteractiveTuiReference(getTui)`：始终转发到当前渲染器的稳定 TUI 句柄。
///
/// 它同时实现 [`Component`] 与 [`Tui`]，因此可以放进 `Rc<RefCell<dyn Tui>>` 交给只依赖
/// `kiana-tui` 的代码。每次调用都重新借用内部的 `Rc<RefCell<InteractiveRenderer>>`，
/// 始终命中**当前**渲染器（模式切换只替换 `RefCell` 的内容）。`children()` 不可用，
/// 请用 [`InteractiveTuiReference::take_children`]；`as_any`/`as_any_mut` 返回自身，
/// 供 [`focus_component_address`] 向下转型。
#[derive(Clone)]
pub struct InteractiveTuiReference {
    renderer: Rc<RefCell<InteractiveRenderer>>,
}

impl InteractiveTuiReference {
    /// 用共享渲染器构造。
    pub fn new(renderer: Rc<RefCell<InteractiveRenderer>>) -> Self {
        InteractiveTuiReference { renderer }
    }

    /// 当前渲染器句柄。
    pub fn renderer(&self) -> Rc<RefCell<InteractiveRenderer>> {
        Rc::clone(&self.renderer)
    }

    /// `mode`。
    pub fn mode(&self) -> TuiMode {
        self.renderer.borrow().mode()
    }

    /// `isViewportTUI(tui)`。
    pub fn is_viewport_tui(&self) -> bool {
        self.renderer.borrow().is_viewport_tui()
    }

    /// `addChild(component)`。
    pub fn add_child(&mut self, child: Box<dyn Component>) {
        self.renderer.borrow_mut().add_child(child);
    }

    /// `removeChild(component)`。
    pub fn remove_child(&mut self, child: &dyn Component) {
        self.renderer.borrow_mut().remove_child(child);
    }

    /// 按下标移除子项。
    pub fn remove_child_at(&mut self, index: usize) -> Option<Box<dyn Component>> {
        self.renderer.borrow_mut().remove_child_at(index)
    }

    /// `clear()`。
    ///
    /// 语义与 [`InteractiveRenderer::clear`] 相同：主屏可能因持焦子项而提前停止。
    pub fn clear(&mut self) {
        self.renderer.borrow_mut().clear();
    }

    /// `[...children]`。
    pub fn take_children(&mut self) -> Vec<Box<dyn Component>> {
        self.renderer.borrow_mut().take_children()
    }

    /// `children.length`（代理无法借出子项列表，只提供长度）。
    pub fn children_len(&mut self) -> usize {
        self.renderer.borrow_mut().children_len()
    }

    /// 在底层渲染器内部借用期间访问顶层子项。
    pub fn with_children(&mut self, f: &mut dyn FnMut(&mut Vec<Box<dyn Component>>)) {
        self.renderer.borrow_mut().with_children(f);
    }

    /// `setLayoutRoot(component)`（仅全屏）。
    pub fn set_layout_root(&mut self, component: Option<Box<dyn Component>>) {
        self.renderer.borrow_mut().set_layout_root(component);
    }

    /// `getFocusedComponent()`。
    pub fn focus(&self) -> Option<FocusTarget> {
        self.renderer.borrow().focus()
    }

    /// `setFocus(target)`。
    pub fn set_focus(&mut self, target: Option<FocusTarget>) {
        self.renderer.borrow_mut().set_focus(target);
    }

    /// `setFocus(component)` 的等价物：用组件地址解析焦点目标。
    ///
    /// 返回是否成功解析（组件在挂载树里）。未挂载时什么都不做——Rust 的焦点是位置，
    /// 无法表达 TS 的"聚焦一个已脱离挂载树的组件引用"（该引用也收不到输入）。
    pub fn set_focus_to_address(&mut self, address: usize) -> bool {
        let mut renderer = self.renderer.borrow_mut();
        let Some(target) = renderer.focus_target_of_address(address) else {
            return false;
        };
        renderer.set_focus(Some(target));
        true
    }

    /// `start()`。
    pub fn start(&mut self) {
        self.renderer.borrow_mut().start();
    }

    /// 宿主循环一步（见 [`InteractiveRenderer::pump_events`]）。
    pub fn pump_events(&mut self) {
        self.renderer.borrow_mut().pump_events();
    }

    /// 宿主渲染决策为 `Now` 时渲染一帧（见 [`InteractiveRenderer::flush_pending_render`]）。
    pub fn flush_pending_render(&mut self) -> bool {
        self.renderer.borrow_mut().flush_pending_render()
    }

    /// `stop(options)`。
    pub fn stop(&mut self, options: TuiStopOptions) {
        self.renderer.borrow_mut().stop(options);
    }

    /// `renderNow(force)`。
    pub fn render_now(&mut self, force: bool) {
        self.renderer.borrow_mut().render_now(force);
    }

    /// `requestRender(force)`。
    ///
    /// 重入保护：输入分发与渲染期间宿主已经持有 `renderer` 的可变借用，组件回调里的
    /// 同步 `requestRender()` 无法再次借用（TS 的属性调用没有这个限制）。与
    /// [`request_render`]（自由函数）一致，借用失败时静默跳过：输入分发结束后
    /// `TuiBase::handle_terminal_input` 会调用 `request_immediate_render()` 调度下一帧。
    pub fn request_render(&mut self, force: bool) {
        if let Ok(mut renderer) = self.renderer.try_borrow_mut() {
            renderer.request_render(force);
        }
    }

    /// `invalidate()`。
    pub fn invalidate(&mut self) {
        self.renderer.borrow_mut().invalidate();
    }

    /// `render(width)`。
    pub fn render(&mut self, width: usize) -> Vec<String> {
        self.renderer.borrow_mut().render(width)
    }

    /// `handleInput(data)`。
    pub fn handle_input(&mut self, data: &str) {
        self.renderer.borrow_mut().handle_input(data);
    }

    /// `terminal`。
    ///
    /// 与 [`InteractiveRenderer::terminal`] 一样返回共享句柄；句柄在模式切换后仍然有效。
    pub fn terminal(&self) -> Rc<RefCell<Box<dyn Terminal>>> {
        self.renderer.borrow().terminal()
    }

    /// `getShowHardwareCursor()`。
    pub fn get_show_hardware_cursor(&self) -> bool {
        self.renderer.borrow().get_show_hardware_cursor()
    }

    /// `setShowHardwareCursor(enabled)`。
    pub fn set_show_hardware_cursor(&mut self, enabled: bool) {
        self.renderer.borrow_mut().set_show_hardware_cursor(enabled);
    }

    /// `getClearOnShrink()`。
    pub fn get_clear_on_shrink(&self) -> bool {
        self.renderer.borrow().get_clear_on_shrink()
    }

    /// `setClearOnShrink(enabled)`。
    pub fn set_clear_on_shrink(&mut self, enabled: bool) {
        self.renderer.borrow_mut().set_clear_on_shrink(enabled);
    }

    /// `captureRenderState()`（仅主屏）。
    pub fn capture_render_state(&self) -> Option<TuiMainScreenRenderState> {
        self.renderer.borrow().capture_render_state()
    }

    /// `restoreRenderState(state)`（仅主屏）。
    pub fn restore_render_state(&mut self, state: TuiMainScreenRenderState) {
        self.renderer.borrow_mut().restore_render_state(state);
    }

    /// `setCopyOnSelect(enabled)`（仅全屏）。
    pub fn set_copy_on_select(&mut self, enabled: bool) {
        self.renderer.borrow_mut().set_copy_on_select(enabled);
    }

    /// `getCopyOnSelect()`。
    pub fn get_copy_on_select(&self) -> bool {
        self.renderer.borrow().get_copy_on_select()
    }

    /// `setWheelScrollLines(lines)`（仅全屏）。
    pub fn set_wheel_scroll_lines(&mut self, lines: WheelScrollLines) {
        self.renderer.borrow_mut().set_wheel_scroll_lines(lines);
    }

    /// `onDebug`（取出）。
    pub fn take_on_debug(&mut self) -> Option<Box<dyn FnMut()>> {
        self.renderer.borrow_mut().take_on_debug()
    }

    /// `onDebug = callback`。
    pub fn set_on_debug(&mut self, callback: Option<Box<dyn FnMut()>>) {
        self.renderer.borrow_mut().set_on_debug(callback);
    }

    /// `hasOverlayEntries`。
    pub fn has_overlay_entries(&self) -> bool {
        self.renderer.borrow().has_overlay_entries()
    }

    /// `hasOverlay()`。
    pub fn has_overlay(&self) -> bool {
        self.renderer.borrow().has_overlay()
    }

    /// `hideOverlay()`。
    pub fn hide_overlay(&mut self) {
        self.renderer.borrow_mut().hide_overlay();
    }

    /// `showOverlay(component, options)`。
    pub fn show_overlay(
        &mut self,
        component: Box<dyn Component>,
        options: OverlayOptions,
    ) -> Box<dyn OverlayHandle> {
        self.renderer.borrow_mut().show_overlay(component, options)
    }

    /// `flash(message, durationMs?)`。
    pub fn flash(&mut self, message: &str, duration_ms: Option<f64>) {
        self.renderer.borrow_mut().flash(message, duration_ms);
    }

    /// `getScreenLines()`。
    pub fn get_screen_lines(&self) -> Vec<String> {
        self.renderer.borrow().get_screen_lines()
    }

    /// `resetTextSelection()`。
    pub fn reset_text_selection(&mut self) {
        self.renderer.borrow_mut().reset_text_selection();
    }

    /// `hasActiveSelection()`。
    pub fn has_active_selection(&self) -> bool {
        self.renderer.borrow().has_active_selection()
    }

    /// `copyActiveSelectionToClipboard()`。
    pub fn copy_active_selection_to_clipboard(
        &mut self,
    ) -> kiana_tui::alt_screen::CopySelectionStatus {
        self.renderer
            .borrow_mut()
            .copy_active_selection_to_clipboard()
    }

    /// `renderRequested` 的共享句柄。
    pub fn render_requested_handle(&self) -> Rc<Cell<bool>> {
        self.renderer.borrow().render_requested_handle()
    }

    /// `addInputListener(listener)`。
    pub fn add_input_listener(&mut self, listener: TuiInputListener) -> InputListenerId {
        self.renderer.borrow_mut().add_input_listener(listener)
    }

    /// `removeInputListener(id)`。
    pub fn remove_input_listener(&mut self, id: InputListenerId) {
        self.renderer.borrow_mut().remove_input_listener(id);
    }

    /// 取出 `render` 的稳定句柄（TS `const originalRender = tui.render`）。
    ///
    /// 每次调用都重新解析当前渲染器，因此模式切换后句柄仍指向新渲染器。
    pub fn capture_render(&self) -> Rc<dyn Fn(usize) -> Vec<String>> {
        let renderer = Rc::clone(&self.renderer);
        Rc::new(move |width| renderer.borrow_mut().render(width))
    }

    /// 取出 `requestRender` 的稳定句柄（TS `const requestRender = tui.requestRender`）。
    pub fn capture_request_render(&self) -> Rc<dyn Fn(bool)> {
        let renderer = Rc::clone(&self.renderer);
        Rc::new(move |force| renderer.borrow_mut().request_render(force))
    }
}

impl Component for InteractiveTuiReference {
    /// 转发到当前渲染器（[`InteractiveRenderer::render`]）。
    fn render(&mut self, width: usize) -> Vec<String> {
        self.renderer.borrow_mut().render(width)
    }

    /// 转发到当前渲染器的 `handleInput`。
    ///
    /// 与 [`InteractiveRenderer::handle_terminal_input`] 不同，这条路径不经过全屏渲染器的
    /// 视口输入处理（搜索、选择等）；处理终端字节应走后者。
    fn handle_input(&mut self, data: &str) {
        self.renderer.borrow_mut().handle_input(data);
    }

    /// 转发到当前渲染器的 `handleMouse`。
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.renderer.borrow_mut().handle_mouse(event)
    }

    /// 按当前变体转发到底层渲染器。
    fn wants_key_release(&self) -> bool {
        match &*self.renderer.borrow() {
            InteractiveRenderer::Regular(renderer) => renderer.wants_key_release(),
            InteractiveRenderer::Fullscreen(renderer) => renderer.wants_key_release(),
        }
    }

    /// 按当前变体转发到底层渲染器。
    fn handles_input(&self) -> bool {
        match &*self.renderer.borrow() {
            InteractiveRenderer::Regular(renderer) => renderer.handles_input(),
            InteractiveRenderer::Fullscreen(renderer) => renderer.handles_input(),
        }
    }

    /// 转发到当前渲染器的 `invalidate`。
    fn invalidate(&mut self) {
        self.renderer.borrow_mut().invalidate();
    }

    /// 返回代理自身（不是底层渲染器）；[`focus_component_address`] 依赖该向下转型。
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    /// 返回代理自身（可变）；[`focus_component_address`] 依赖该向下转型。
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    /// 代理本身不参与聚焦（返回 `None`）；焦点始终由底层渲染器维护。
    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        None
    }
}

impl Tui for InteractiveTuiReference {
    /// 当前渲染器的模式（[`InteractiveRenderer::mode`]）。
    fn mode(&self) -> TuiMode {
        self.renderer.borrow().mode()
    }

    /// 不可用：代理无法穿过 `Rc<RefCell<…>>` 借出底层子项列表。请用
    /// [`InteractiveTuiReference::take_children`]（TS 侧唯一使用点是 `[...children]`）。
    fn children(&mut self) -> &mut Vec<Box<dyn Component>> {
        panic!("InteractiveTuiReference::children is unavailable; use take_children() instead")
    }

    /// 转发到 [`InteractiveTuiReference::children_len`]，避免代理路径调用会 panic 的
    /// [`Tui::children`]。
    fn children_len(&mut self) -> usize {
        InteractiveTuiReference::children_len(self)
    }

    /// 转发到底层渲染器的借用窗口，供扩展 UI 遍历/删除子项。
    fn with_children(&mut self, f: &mut dyn FnMut(&mut Vec<Box<dyn Component>>)) {
        InteractiveTuiReference::with_children(self, f)
    }

    /// 转发到当前渲染器（[`InteractiveRenderer::add_child`]）。
    fn add_child(&mut self, child: Box<dyn Component>) {
        self.renderer.borrow_mut().add_child(child);
    }

    /// 转发到当前渲染器的 `removeChild`。
    fn remove_child(&mut self, child: &dyn Component) {
        self.renderer.borrow_mut().remove_child(child);
    }

    /// 转发到当前渲染器的 `removeChildAt`。
    fn remove_child_at(&mut self, index: usize) -> Option<Box<dyn Component>> {
        self.renderer.borrow_mut().remove_child_at(index)
    }

    /// 转发到当前渲染器的 `clear`；主屏可能因持焦子项而提前停止（见
    /// [`InteractiveRenderer::clear`]）。
    fn clear_children(&mut self) {
        self.renderer.borrow_mut().clear();
    }

    /// 当前焦点目标（[`InteractiveRenderer::focus`]）。
    fn focus(&self) -> Option<FocusTarget> {
        self.renderer.borrow().focus()
    }

    /// 设置当前渲染器的焦点（[`InteractiveRenderer::set_focus`]）。
    fn set_focus(&mut self, target: Option<FocusTarget>) {
        self.renderer.borrow_mut().set_focus(target);
    }

    /// 转发到当前渲染器的 `showOverlay`；返回的句柄由底层实现持有生命周期。
    fn show_overlay(
        &mut self,
        component: Box<dyn Component>,
        options: OverlayOptions,
    ) -> Box<dyn OverlayHandle> {
        self.renderer.borrow_mut().show_overlay(component, options)
    }

    /// 转发到当前渲染器的 `hideOverlay`。
    fn hide_overlay(&mut self) {
        self.renderer.borrow_mut().hide_overlay();
    }

    /// 当前渲染器是否有可见 overlay（[`InteractiveRenderer::has_overlay`]）。
    fn has_overlay(&self) -> bool {
        self.renderer.borrow().has_overlay()
    }

    /// 转发到当前渲染器的 `start`。
    fn start(&mut self) {
        self.renderer.borrow_mut().start();
    }

    /// 转发到当前渲染器的 `stop`；`options` 原样传给底层实现。
    fn stop(&mut self, options: TuiStopOptions) {
        self.renderer.borrow_mut().stop(options);
    }

    /// 转发到当前渲染器的 `renderNow`。
    fn render_now(&mut self, force: bool) {
        self.renderer.borrow_mut().render_now(force);
    }

    /// 转发到当前渲染器的 `requestRender`。
    ///
    /// 注意本 trait 实现使用 `borrow_mut()`；同名的固有方法
    /// [`InteractiveTuiReference::request_render`] 为可重入场景改用 `try_borrow_mut()`。
    /// 通过 `dyn Tui` 调用时，若渲染器已被借用会 panic。
    fn request_render(&mut self, force: bool) {
        self.renderer.borrow_mut().request_render(force);
    }

    /// 转发到当前渲染器的 `addInputListener`。
    fn add_input_listener(&mut self, listener: TuiInputListener) -> InputListenerId {
        self.renderer.borrow_mut().add_input_listener(listener)
    }

    /// 转发到当前渲染器的 `removeInputListener`；未知 id 为 no-op（由底层实现决定）。
    fn remove_input_listener(&mut self, id: InputListenerId) {
        self.renderer.borrow_mut().remove_input_listener(id);
    }

    /// 查询终端颜色：内部 future 在整个查询期间持有渲染器的可变借用。
    ///
    /// 因此 `timeout_ms` 到期前不能有其它借用者；本 crate 在单线程 `LocalSet` 上运行，
    /// 查询期间没有并发借用（与 `TuiBase` 的 pending 查询模型一致）。
    // 内部 future 借用 `&mut self`（查询期间需要持续访问渲染器）。单线程 + LocalSet 下
    // 没有并发借用者；与 TuiBase 的 pending 查询一致。
    #[allow(
        clippy::await_holding_refcell_ref,
        reason = "the inner query future borrows the renderer for its whole lifetime; no concurrent borrower exists on the LocalSet thread"
    )]
    fn query_terminal_colors(
        &mut self,
        timeout_ms: u64,
    ) -> Pin<Box<dyn std::future::Future<Output = TerminalColors> + '_>> {
        let renderer = Rc::clone(&self.renderer);
        Box::pin(async move {
            let mut renderer = renderer.borrow_mut();
            match &mut *renderer {
                InteractiveRenderer::Regular(renderer) => {
                    renderer.query_terminal_colors(timeout_ms).await
                }
                InteractiveRenderer::Fullscreen(renderer) => {
                    renderer.query_terminal_colors(timeout_ms).await
                }
            }
        })
    }

    /// 只有全屏渲染器返回 `true`（[`InteractiveRenderer::is_viewport_tui`]）。
    fn is_viewport_tui(&self) -> bool {
        self.renderer.borrow().is_viewport_tui()
    }
}

/// `createInteractiveTuiReference(getTui)`。
pub fn create_interactive_tui_reference(
    renderer: Rc<RefCell<InteractiveRenderer>>,
) -> InteractiveTuiReference {
    InteractiveTuiReference::new(renderer)
}

// ---------------------------------------------------------------------------
// 共享组件代理
// ---------------------------------------------------------------------------

/// `Rc<RefCell<dyn Component>>` 的容器代理。
///
/// TS 的 `Container.addChild(component)` 保存组件引用；Rust 的 `Box<dyn Component>` 独占
/// 所有权，需要"同一组件既在容器里又能被宿主改"时用本代理挂载（渲染/输入/焦点全部转发
/// 到共享句柄）。`as_any` 返回代理自身，测试据此比较底层 `Rc` 的身份。
///
/// 典型用途：交互模式把同一组件既作为渲染器的子项（由容器拥有），又保存在自己的字段里
/// 以便后续更新。代理不复制状态，每个方法都借用底层 [`SharedComponent`]。
///
/// 借用约定：转发方法用 `borrow()`/`borrow_mut()`，底层正被借用时会 panic；
/// `as_focusable` 用 `try_borrow_mut()`，探测失败按“不可聚焦”处理。`as_any` 返回的是
/// **代理**而不是底层组件，调用方需先下转成本类型再取
/// [`SharedComponentProxy::component`]。
pub struct SharedComponentProxy(SharedComponent);

impl SharedComponentProxy {
    /// 包装一个共享组件（不复制，之后与其它持有者共享同一 `Rc`）。
    pub fn new(component: SharedComponent) -> Self {
        SharedComponentProxy(component)
    }

    /// 底层共享句柄。
    pub fn component(&self) -> &SharedComponent {
        &self.0
    }
}

impl Component for SharedComponentProxy {
    /// 转发到底层组件的 `render`。
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }

    /// 转发到底层组件的 `handleInput`。
    fn handle_input(&mut self, data: &str) {
        self.0.borrow_mut().handle_input(data);
    }

    /// 转发到底层组件的 `handleMouse`。
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.0.borrow_mut().handle_mouse(event)
    }

    /// 转发到底层组件的 `wants_key_release`。
    fn wants_key_release(&self) -> bool {
        self.0.borrow().wants_key_release()
    }

    /// 转发到底层组件的 `handles_input`（TS `"handleInput" in component` 的判定）。
    fn handles_input(&self) -> bool {
        self.0.borrow().handles_input()
    }

    /// 转发到底层组件的 `invalidate`。
    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }

    /// 透明转发 TS 的可选 `dispose()`（宿主替换/清空组件时调用）。
    fn dispose(&mut self) {
        self.0.borrow_mut().dispose();
    }

    /// 返回代理自身；调用方下转成本类型后可用 [`SharedComponentProxy::component`] 取底层
    /// 句柄（这也是 `shared_child_identity` 等调用点的识别方式）。
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    /// [`SharedComponentProxy::as_any`] 的可变版本。
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    /// 只有底层组件可获得焦点时，代理才返回 `Some(self)`（`isFocusable` 语义）。
    ///
    /// 用 `try_borrow_mut` 探测：底层正被借用时按不可聚焦处理，避免焦点簿记触发 panic。
    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        // 只有底层组件可获得焦点时，代理才报告自己可实现焦点（`isFocusable` 语义）。
        let focusable = self
            .0
            .try_borrow_mut()
            .map(|mut component| component.as_focusable().is_some())
            .unwrap_or(false);
        focusable.then_some(self as &mut dyn Focusable)
    }

    /// 把容器子项遍历透传到底层组件（让焦点/挂载路径能穿过代理）。
    fn for_each_container_child(&self, f: &mut dyn FnMut(&dyn Component) -> bool) {
        self.0.borrow().for_each_container_child(f);
    }

    /// [`SharedComponentProxy::for_each_container_child`] 的可变版本。
    fn for_each_container_child_mut(
        &mut self,
        f: &mut dyn FnMut(&mut (dyn Component + 'static)) -> bool,
    ) {
        self.0.borrow_mut().for_each_container_child_mut(f);
    }

    /// 转发底层组件的布局节点（每次调用重新构造，见 `Component::layout_node`）。
    fn layout_node(&self) -> Option<kiana_tui::layout_node::LayoutNode> {
        self.0.borrow().layout_node()
    }
}

impl Focusable for SharedComponentProxy {
    /// 读取底层组件的焦点标志；底层不可聚焦时返回 `false`。
    ///
    /// 用 `borrow_mut()` 是因为 `as_focusable` 需要可变借用；借用冲突会 panic。
    fn focused(&self) -> bool {
        self.0
            .borrow_mut()
            .as_focusable()
            .map(|focusable| focusable.focused())
            .unwrap_or(false)
    }

    /// 设置底层组件的焦点标志；底层不可聚焦时无操作。
    fn set_focused(&mut self, focused: bool) {
        if let Some(focusable) = self.0.borrow_mut().as_focusable() {
            focusable.set_focused(focused);
        }
    }
}

/// `RequestRenderCallback`：从 `Rc<RefCell<dyn Tui>>` 句柄转发 `requestRender()`。
///
/// TS 的回调直接调用 `ui.requestRender()`；Rust 的句柄可能在极少数重入场景里已被借用
/// （例如渲染期间触发的定时器），此时静默跳过而不是 panic。
///
/// 只保护外层 `Rc<RefCell<dyn Tui>>` 的借用；内层渲染器的借用策略由被转发的 `Tui`
/// 实现决定。
pub fn request_render_callback(ui: &Rc<RefCell<dyn Tui>>) -> RequestRenderCallback {
    let ui = Rc::clone(ui);
    Rc::new(move || {
        if let Ok(mut ui) = ui.try_borrow_mut() {
            ui.request_render(false);
        }
    })
}

/// 组件地址（`&dyn Component` 的数据指针）。
///
/// 用作同一挂载树内的临时身份：`Box` 移动不会改变该地址，因此可以在迁移组件时保存；
/// 但组件释放后地址可能被新分配复用，不能跨生命周期保存。
pub fn component_address(component: &dyn Component) -> usize {
    component as *const dyn Component as *const () as usize
}

/// `setFocus(component)`：按组件地址设置焦点（交互模式与组件单元共用的辅助函数）。
///
/// 句柄可能是 [`InteractiveTuiReference`]（生产路径），也可能是具体渲染器（测试）；
/// 只有代理能解析地址之外的挂载关系，其余实现无操作（与"未挂载组件无法聚焦"一致）。
///
/// 判定方式是对 `as_any_mut` 的结果向下转型：只有能转成 [`InteractiveTuiReference`]
/// 的句柄才会生效，外层句柄正被借用时静默返回。
pub fn focus_component_address(ui: &Rc<RefCell<dyn Tui>>, address: usize) {
    let Ok(mut ui) = ui.try_borrow_mut() else {
        return;
    };
    if let Some(reference) = ui
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<InteractiveTuiReference>())
    {
        reference.set_focus_to_address(address);
    }
}

/// 请求一帧（`requestRender()`）。
///
/// 外层句柄借用失败时静默跳过；实际转发到渲染器的行为由具体 `Tui` 实现决定。
pub fn request_render(ui: &Rc<RefCell<dyn Tui>>) {
    if let Ok(mut ui) = ui.try_borrow_mut() {
        ui.request_render(false);
    }
}

/// 编辑器槽位：`editorContainer` + 当前 editor 的共享句柄。
///
/// TS 的 `showOverlay` / `restoreEditor` 直接对容器 `clear()`/`addChild()` 并 `setFocus`；
/// Rust 用共享容器复刻同一流程。焦点按组件地址解析（见 [`focus_component_address`]）。
///
/// `container` 是交互模式下挂载 editor 的那个容器。`show` 清空后挂载临时组件（overlay
/// 内容），`restore` 清空后重新挂载共享的 `editor`。焦点取容器**最后一个**子项的地址，
/// 因此流程假定每次只挂一个子项；若容器里还有别的子项，焦点会落到最后挂载的那个。
#[derive(Clone)]
pub struct EditorSlot {
    /// 交互模式的 TUI 句柄。
    pub ui: Rc<RefCell<dyn Tui>>,
    /// `editorContainer`。
    pub container: Rc<RefCell<Container>>,
    /// 当前编辑器（恢复时重新挂载）。
    pub editor: SharedComponent,
}

impl EditorSlot {
    /// 用共享句柄构造。
    pub fn new(
        ui: Rc<RefCell<dyn Tui>>,
        container: Rc<RefCell<Container>>,
        editor: SharedComponent,
    ) -> Self {
        EditorSlot {
            ui,
            container,
            editor,
        }
    }

    /// 取出编辑器槽位所需的全部句柄（避免重复克隆）。
    pub fn from_slot(slot: &EditorSlot) -> Self {
        EditorSlot::new(
            Rc::clone(&slot.ui),
            Rc::clone(&slot.container),
            Rc::clone(&slot.editor),
        )
    }

    /// 最近挂载子项的地址（用于 `setFocus`）。
    fn last_child_address(&self) -> Option<usize> {
        let container = self.container.borrow();
        container
            .children
            .last()
            .map(|child| component_address(child.as_ref()))
    }

    /// `showOverlay(component)`：`editorContainer.clear()` → `addChild` → `setFocus` →
    /// `requestRender()`。
    ///
    /// 焦点解析失败（句柄无法按地址定位）时仍会请求渲染；容器只在这个同步
    /// 块内被借用，随后立即释放。
    pub fn show(&self, component: Box<dyn Component>) {
        {
            let mut container = self.container.borrow_mut();
            container.clear();
            container.add_child(component);
        }
        if let Some(address) = self.last_child_address() {
            focus_component_address(&self.ui, address);
        }
        request_render(&self.ui);
    }

    /// `restoreEditor()`：`editorContainer.clear()` → `addChild(editor)` → `setFocus`。
    ///
    /// 这里**不**请求渲染：调用方对收尾步骤的需求不同——`bug-report.ts` 的
    /// `restoreEditor` 末尾有 `requestRender()`，而 `session-share.ts` 的没有，因此由
    /// 调用方补（见 [`restore_editor_with_loader`] 与 `bug_report` 的 overlay 收尾逻辑）。
    ///
    /// 恢复的编辑器以新的 [`SharedComponentProxy`] 挂回容器；底层 `editor` 句柄不变，
    /// 因此其它持有者仍能看到同一实例。
    pub fn restore(&self) {
        {
            let mut container = self.container.borrow_mut();
            container.clear();
            container.add_child(Box::new(SharedComponentProxy::new(Rc::clone(&self.editor))));
        }
        if let Some(address) = self.last_child_address() {
            focus_component_address(&self.ui, address);
        }
    }
}

// ---------------------------------------------------------------------------
// BorderedLoader 共享代理
// ---------------------------------------------------------------------------

/// 当前主题（[`BorderedLoader`](kiana_ui_kit::bordered_loader::BorderedLoader) 需要 `Arc<Theme>`）。
///
/// 优先全局当前主题；未初始化时退回内置 `"dark"`，两者都取不到才 panic。因此加载器
///（例如 `bug_report` / `session_share` 的进度框）可以在主题初始化之前的路径上构造。
///
/// 具体来源依次是：当前主题（[`kiana_theme::current_theme`]）→ 内置 `"dark"` 主题
/// （[`kiana_theme::get_theme_by_name`]）；Rust 在两者都取不到时才 panic。与 TS 不同，
/// 主题未初始化时这里退回内置 dark 主题而不抛错。
pub fn loader_theme() -> std::sync::Arc<kiana_theme::Theme> {
    kiana_theme::current_theme()
        .or_else(|| kiana_theme::get_theme_by_name("dark"))
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// 创建可取消的 `BorderedLoader`（TS `new BorderedLoader(ui, theme, message)`）。
///
/// 返回共享句柄：调用方既要把代理挂进容器，又要读 `signal()` / 调 `dispose()`。
///
/// 使用全局主题与默认选项（`cancellable = true`），并把 `request_render` 接到传入的
/// TUI 句柄（借用失败静默跳过）。返回的 `Rc` 与挂进容器的 [`SharedLoader`] 指向同一
/// 实例：在任一侧 `dispose()` 都作用于同一个加载器。
pub fn create_bordered_loader(
    ui: &Rc<RefCell<dyn Tui>>,
    message: &str,
) -> Rc<RefCell<kiana_ui_kit::bordered_loader::BorderedLoader>> {
    let request_render = request_render_callback(ui);
    Rc::new(RefCell::new(
        kiana_ui_kit::bordered_loader::BorderedLoader::new(
            Some(request_render),
            loader_theme(),
            message,
            None,
        ),
    ))
}

/// 共享 [`BorderedLoader`](kiana_ui_kit::bordered_loader::BorderedLoader) 的组件代理。
///
/// 与 `kiana_ui_kit` 内部私有的同名类型不是一回事：那个代理只把加载器挂进
/// `BorderedLoader` 自己的容器，本类型用于把**整个** `BorderedLoader` 共享给调用方。
///
/// 这里只覆写渲染、输入、失效、`dispose` 与 `as_any`/`as_any_mut`（返回代理自身，
/// 而非 [`Component`] 默认的 `None`）；其余方法沿用 [`Component`] 默认实现
///（例如 `handle_mouse` 返回 `None`、`handles_input` 默认为 `true`）。
pub struct SharedLoader(Rc<RefCell<kiana_ui_kit::bordered_loader::BorderedLoader>>);

impl SharedLoader {
    /// 包装共享加载器（不复制，与调用方共享同一个 `Rc`）。
    pub fn new(loader: Rc<RefCell<kiana_ui_kit::bordered_loader::BorderedLoader>>) -> Self {
        SharedLoader(loader)
    }

    /// 底层共享句柄。
    pub fn loader(&self) -> &Rc<RefCell<kiana_ui_kit::bordered_loader::BorderedLoader>> {
        &self.0
    }
}

impl Component for SharedLoader {
    /// 转发到加载器的 `render`。
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }

    /// 转发到加载器的 `handleInput`（取消键等由加载器自己处理）。
    fn handle_input(&mut self, data: &str) {
        self.0.borrow_mut().handle_input(data);
    }

    /// 转发到加载器的 `invalidate`。
    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }

    /// 透明转发 TS 的可选 `dispose()`（宿主替换/清空组件时调用）。
    fn dispose(&mut self) {
        self.0.borrow_mut().dispose();
    }

    /// 返回代理自身；调用方下转成本类型后可用 [`SharedLoader::loader`] 取底层句柄。
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    /// [`SharedLoader::as_any`] 的可变版本。
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

/// `restoreEditor(loader)`（`bug-report.ts` 版本）：`loader.dispose()` → 恢复编辑器 →
/// `requestRender()`。
pub fn restore_editor_with_loader(
    slot: &EditorSlot,
    loader: &Rc<RefCell<kiana_ui_kit::bordered_loader::BorderedLoader>>,
) {
    loader.borrow_mut().dispose();
    slot.restore();
    request_render(&slot.ui);
}
