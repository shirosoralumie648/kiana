//! theme-controller.ts。
//!
//! TS: `packages/coding-agent/src/modes/interactive/theme/theme-controller.ts`（253 行）。
//!
//! 控制器把「主题设置」与「终端实际上报的颜色/明暗」绑在一起：主题立即应用，终端的颜色在
//! 查询完成后更新它（system 主题在此之前渲染为灰阶）。启动横幅之类把主题颜色烤进字符串的
//! 内容，可以 `wait_for_terminal_colors()` 等颜色到达后再生成。
//!
//! ## 与 TS 的异步适配
//!
//! TS 里 `ui` 是一个对象，控制器任意方法都能 `this.ui.invalidate()`；Rust 的 host 是
//! `&mut dyn ThemeHost`，不能存进控制器（也会和「host 调用监听器」形成重入）。因此：
//!
//! - 控制器的方法都接收 `host`；[`ThemeHost`] 只抽 TS 控制器真正用到的成员。
//! - 终端颜色查询的 future 进入控制器的队列：`apply_from_settings()` **同步**发起查询，并在
//!   tokio 运行时上下文里先轮询一次 —— 已经就绪的结果当场应用（同步轮询，无需 await），
//!   因此丢弃返回的 future 不会丢**已就绪**的结果。未就绪的查询留
//!   在队列里；下一次 `wait_for_terminal_colors()` / `apply_from_settings()` 的 future 被驱动
//!   时，等待**最新**查询（与 TS 的 `waitForTerminalColors()` 相同），并对其余查询做一次
//!   非阻塞检查（被新查询取代的旧结果也会补上）。
//! - 终端的 `onLateReply` 与明暗方案监听器由 host 在**自己的**回调栈里触发，此时无法再借
//!   host。它们同步更新全局主题与控制器状态（颜色、明暗、`dim` 混色等立即生效，对应 TS
//!   在回调里同步执行的部分），并把重绘拆成两半：`requestRender()` 通过
//!   [`ThemeHost::render_requested_handle`] 立即置位宿主的调度标志（TS 也是同步调用），
//!   `invalidate()` 记在 `ui_dirty` 上，由下一次 [`InteractiveThemeController::pump`] /
//!   [`InteractiveThemeController::wait_for_terminal_colors`] /
//!   [`InteractiveThemeController::apply_from_settings`] 驱动时补上。
//! - 明暗方案变化触发的重新查询被记成 `query_requested`，由下一次
//!   [`InteractiveThemeController::pump`] / `wait_for_terminal_colors()` 真正发起（TS 在
//!   监听器里立即发起）。
//!
//! ## 事件循环契约（生产调用方）
//!
//! 回调栈里无法调用 host 的 `query_terminal_colors()`/`invalidate()`（`kiana-tui` 在持有
//! `&mut TuiBase` 时调用监听器，见下面的集成缺口）。因此宿主事件循环应当在**每次
//! `handle_terminal_input()` 之后、渲染之前**调用一次
//! `controller.wait_for_terminal_colors(&mut host)`（它的 future 可以丢弃；发起查询、补
//! `invalidate()`、应用已就绪结果都在调用瞬间完成）。这样终端明暗切换在同一个事件循环
//! 迭代里就会重新查询并应用颜色，与 TS 监听器里的同步语义一致；启动路径则按
//! `apply_from_settings()` + `await wait_for_terminal_colors()` 使用。事件循环不想在每次
//! 输入后 `await` 时，可以改调同步的 [`InteractiveThemeController::pump`]（它只做同一批
//! deferred 动作与一次非阻塞轮询）。
//!
//! ## 集成缺口（不在本 crate 修复）
//!
//! 1. 本模块只为 `TuiBase`/`TuiMainScreen` 实现了 [`ThemeHost`]；`TuiAltScreen` 自己已暴露
//!    `base()`/`base_mut()`（`kiana-tui/src/alt_screen.rs`），外部可以适配它——
//!    `kiana-interactive` 的 `RendererThemeHost` 就同时覆盖 regular（`TuiMainScreen`）与
//!    fullscreen（`TuiAltScreen`）两种渲染器。
//! 2. `kiana-tui` 的监听器/迟到回复回调运行在宿主 `&mut self` 内部，没有重入句柄，因此回调里
//!    无法**发起**新的颜色查询或 `invalidate()`。TS 在监听器里同步调用
//!    `queryTerminalColors()`；Rust 只能把查询排到下一次驱动（见「事件循环契约」）。要逐字
//!    复刻需要 `kiana-tui` 在回调里提供可重入的查询/失效入口。
//!
//! ## 一次完整流程（`apply_from_settings()` + `wait_for_terminal_colors()`）
//!
//! 1. 读设置：`current_theme_setting`（构造参数或上一次显式设置）优先，其次是宿主的
//!    `get_theme_setting()`；`resolve_theme_setting()` 把 `"light/dark"` 之类的 auto 形式
//!    按当前终端明暗解析成具体名字，解析失败（含带非法 `/`）回退 `system`。
//! 2. 决定 auto-sync：设置能被解析成 auto 形式，或解析出的名字是 `system` 时开启明暗方案
//!    通知；随后 `set_theme(name, true)` 换全局主题（第二个参数 `true` 会启动主题文件
//!    watcher），更新 `active_theme_name` 并调用宿主的 `on_changed`。
//! 3. 同步发起一次终端颜色查询（`start_query`）：在 tokio 运行时上下文里先轮询一次，已就绪
//!    就立即合并并应用；未就绪的 future 进入 `pending_queries`。
//! 4. 驱动阶段先补发监听器排队的查询与 deferred 重绘，再等待**最新**查询完成，把颜色合并进
//!    控制器并写入全局 `set_terminal_colors()`；最后由 `reapply_for_terminal()` 决定是否
//!    换主题：`system` 总是重建（它由终端颜色生成），其它主题只在解析结果变化时重建。
//!
//! ## 状态与不变量
//!
//! - `ControllerState` 用 `Rc<RefCell<…>>` 共享：late reply 与明暗监听器在 host 的回调栈里
//!   同步运行，必须能更新控制器状态，但拿不到 `&mut host`。
//! - `active_theme_name` 是控制器认为「已经应用」的主题名；`"<in-memory>"` 表示
//!   `set_theme_instance()` 装入的实例，此后终端颜色/明暗变化不会再替换它。
//! - `current_theme_setting` 一旦被设置（构造参数、成功的 `set_theme_name`、
//!   `set_theme_setting`），就会**遮蔽** `get_theme_setting`：读取设置的两条路径都先看缓存，
//!   缓存为 `Some` 时不再调用宿主的 getter。
//! - `ui_dirty` / `query_requested` / `render_requested` 分别承担：延后的
//!   `invalidate()`+`requestRender()`、延后的颜色查询、回调栈内立即置位的渲染请求。
//!   前两者只能由驱动路径（[`InteractiveThemeController::pump`] /
//!   [`InteractiveThemeController::wait_for_terminal_colors`] /
//!   [`InteractiveThemeController::apply_from_settings`]）消费；`render_requested`
//!   由回调置位后由宿主的渲染调度器消费（`TuiBase::take_redraw_request`）。
//!
//! ## 错误与失败模式
//!
//! - 主题名解析失败或加载失败：`set_theme` 内部回退 `system`；控制器把
//!   `active_theme_name` 记为 `system`。只有「设置里显式写了一个主题」时（`show_error`）
//!   才把错误文案交给宿主，文案逐字为
//!   `Failed to load theme "<name>": <error>` + 换行 + `Fell back to the system theme.`
//!   （测试 `reports_failed_explicit_themes_with_the_fallback_message` 锁定）。
//! - 终端颜色查询超时：host 返回截止时刻的部分结果（可能为空），控制器按字段合并旧值，
//!   因此不会把已知颜色抹掉；合并结果与旧值相同时跳过重绘。
//! - 颜色查询永远不完成（测试里的 gated 假实现）：`pending_queries` 中的旧查询只做非阻塞
//!   轮询，只有「最新」查询会被 `await`，等待不会卡在已被取代的查询上。
//!
//! ## 线程与运行时约束
//!
//! 控制器本身 `!Send`（含 `Rc`/`RefCell` 与不带 `Send` 的回调），只能在单线程执行器（如
//! tokio `LocalSet`）里使用。`start_query`/`poll_queued_queries` 在没有 tokio 运行时上下文
//! 时不做任何轮询：查询 future 依赖 tokio 定时器，只能在运行时里 poll。轮询使用
//! `Waker::noop()`——控制器不依赖唤醒通知，进度完全由调用方的下一次驱动推进。

use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use kiana_tui::tui::TuiBase;
use kiana_tui::{RgbColor, TerminalColorScheme, TerminalColors};

use crate::theme::{
    SYSTEM_THEME_NAME, Theme, ThemeResult, get_terminal_theme, init_theme,
    mark_terminal_colors_pending, parse_auto_theme_setting, resolve_theme_setting,
    set_terminal_color_scheme, set_terminal_colors, set_theme, set_theme_instance,
};

/// `TERMINAL_QUERY_TIMEOUT_MS = 100`。
///
/// system 主题在终端回色之前保持灰阶的时间：终端会在颜色回复之后紧接着回 DA1，所以只有
/// 两者都不回的终端才会用满这个超时；更晚到达的回复仍然会生效。
///
/// 它是 `requestTerminalColors` 与控制器查询共用的固定值，调用方没有覆盖入口；host 从
/// 收到调用时开始计时，超时后以截止时刻的部分结果完成 future（迟到回复仍走 late-reply
/// 回调，见 [`ThemeHost::query_terminal_colors`]）。
pub const TERMINAL_QUERY_TIMEOUT_MS: u64 = 100;

/// 控制器使用 host 的方式（TS: `TUI` 的六个成员）。
///
/// 之所以需要这个 trait：`kiana_tui::Tui` 只冻结了 `query_terminal_colors(timeout_ms)`（没有
/// `onLateReply`），明暗方案通知是 `TuiBase` 的固有方法；`TuiAltScreen` 也没有本模块内的
/// 实现（它已暴露 `base_mut()`，外部可自行适配）。实现见本模块末尾的 `TuiBase` /
/// `TuiMainScreen`。
///
/// trait 是对象安全的，控制器只持有 `&mut dyn ThemeHost`。[`ThemeHost::query_terminal_colors`]
/// 返回 `'static` future 而不是借用 host 的 future，这是硬约束：查询 future 会被存进控制器
/// 的 `pending_queries`，在后续任意一次驱动里轮询，那时 host 已经被调用方重新借用。实现者
/// 必须把查询需要的终端状态（`TuiBase` 用 `Rc<RefCell<…>>`）移进 future，不能捕获
/// `&mut self`。
pub trait ThemeHost {
    /// TS: `ui.queryTerminalColors({ timeoutMs, onLateReply })`。
    ///
    /// 查询失败等同于「终端没有上报颜色」：返回默认（空）颜色。
    ///
    /// 超时语义（`TuiBase` 的实现如此，见其 `query_terminal_colors_with_late_reply`）：
    /// `timeout_ms` 从调用时刻起算，future 在截止时刻以已收集的部分结果完成；截止后到达的
    /// 回复通过 `on_late_reply` 再交付一次。调用方不能假设 future 完成后不再有回调——控制器
    /// 依赖迟到回复来补上超时后的颜色。
    fn query_terminal_colors(
        &mut self,
        timeout_ms: u64,
        on_late_reply: Option<Box<dyn FnMut(TerminalColors)>>,
    ) -> Pin<Box<dyn Future<Output = TerminalColors> + 'static>>;

    /// TS: `ui.setTerminalColorSchemeNotifications(enabled)`。
    ///
    /// 控制 host 是否向终端开启方案上报（`TuiBase` 写 `?2031h`/`?2031l`）；它不负责过滤
    /// 已注册的监听器。
    fn set_scheme_notifications(&mut self, enabled: bool);

    /// TS: `ui.onTerminalColorSchemeChange(listener)` → 取消用的 id。
    ///
    /// 回调在 host 收到终端上报时同步执行（`TuiBase` 在 `handle_terminal_input` 内部调用），
    /// 返回的 id 必须原样交给 [`ThemeHost::remove_scheme_listener`]。
    fn add_scheme_listener(&mut self, listener: Box<dyn FnMut(TerminalColorScheme)>) -> usize;

    /// TS: `unsubscribe()`。
    ///
    /// 传入 [`ThemeHost::add_scheme_listener`] 返回的 id；未知 id 应被忽略（`TuiBase` 用
    /// `retain` 实现）。
    fn remove_scheme_listener(&mut self, id: usize);

    /// TS: `ui.invalidate()`。
    ///
    /// 使已挂载组件失效（`TuiBase` 映射到 `invalidate_all`，即所有根与 overlay 组件）。
    fn invalidate(&mut self);

    /// TS: `ui.requestRender()`。
    ///
    /// 请求一次差分渲染；控制器从不请求强制全量渲染。
    fn request_render(&mut self);

    /// TS: `ui.requestRender()` 的**共享句柄**，可在 host 的回调栈里安全使用
    /// （`TuiBase::render_requested_handle()` 的等价物）。
    ///
    /// late reply / 明暗监听器由 host 在自己的 `&mut self` 方法里调用，控制器无法再借
    /// host；有了这个句柄，回调里的 `requestRender()` 可以像 TS 一样立即置位宿主的调度
    /// 标志（`invalidate()` 仍需下一次驱动补上，见模块文档）。默认没有句柄时退化为
    /// “下一次驱动时补一次 invalidate+requestRender”。
    ///
    /// 实现约定：返回的 `Cell` 必须就是 host 的 `request_render()` 所置位的那个标志，
    /// 否则回调路径的请求对宿主调度器不可见（`TuiBase` 正是如此）。
    fn render_requested_handle(&self) -> Option<Rc<std::cell::Cell<bool>>> {
        None
    }
}

/// TS: `requestTerminalColors(ui, apply)`。
///
/// 查询终端颜色，在完成或超时时调用 `apply`，超时后到达的回复也会再次调用。查询失败等同
/// 于终端不上报颜色（`apply({})`）。返回的 future 在第一次 `apply` 之后完成。
///
/// `apply` 通过 `Rc<RefCell<…>>` 同时交给 late-reply 回调：如果迟到回复恰好发生在上面的
/// future 正在执行 `apply` 期间（`try_borrow_mut` 失败），这次迟到回复会被**丢弃**而不是
/// 排队。超时固定为 [`TERMINAL_QUERY_TIMEOUT_MS`]，调用方没有超时参数。
///
/// `kiana-coding-agent` 的启动横幅（`cli/startup_ui.rs`）用它单独查询颜色；控制器自己另有
/// 查询队列，不走这个函数。
pub fn request_terminal_colors(
    host: &mut dyn ThemeHost,
    apply: Box<dyn FnMut(TerminalColors) + 'static>,
) -> Pin<Box<dyn Future<Output = ()> + 'static>> {
    let shared = Rc::new(RefCell::new(apply));
    let late = Rc::clone(&shared);
    let query = host.query_terminal_colors(
        TERMINAL_QUERY_TIMEOUT_MS,
        Some(Box::new(move |colors| {
            if let Ok(mut apply) = late.try_borrow_mut() {
                apply(colors);
            }
        })),
    );
    Box::pin(async move {
        let colors = query.await;
        if let Ok(mut apply) = shared.try_borrow_mut() {
            apply(colors);
        }
    })
}

/// TS: `sameRgb(a, b)`。
///
/// `Option` 的“都没有”视为相等；一个有一个没有视为不同。
fn same_rgb(first: Option<RgbColor>, second: Option<RgbColor>) -> bool {
    match (first, second) {
        (None, None) => true,
        (Some(first), Some(second)) => first == second,
        _ => false,
    }
}

/// TS: `sameTerminalColors(a, b)`。
///
/// 逐字段比较；`palette` 连**长度与顺序**一起比较（颜色索引 0-15 的位置有语义）。
fn same_terminal_colors(first: &TerminalColors, second: &TerminalColors) -> bool {
    if !same_rgb(first.foreground, second.foreground)
        || !same_rgb(first.background, second.background)
    {
        return false;
    }
    match (&first.palette, &second.palette) {
        (None, None) => true,
        (Some(first), Some(second)) => {
            first.len() == second.len()
                && first
                    .iter()
                    .zip(second.iter())
                    .all(|(first, second)| first == second)
        }
        _ => false,
    }
}

/// TS: `InteractiveThemeController` 的选项。
///
/// 四个成员在构造时被移进控制器；闭包由控制器内部调用，因此可以捕获 `Rc`（不要求
/// `Send`/`Sync`）。宿主只需保证：`get_theme_setting` 返回当前设置、`show_error` 能展示
/// 完整文案、`on_changed` 在主题每次应用后刷新依赖主题的 UI 状态。
pub struct ThemeControllerOptions {
    /// TS: `getSettingsManager().getThemeSetting()`（只读，避免把 `kiana-config` 暴露进公开类型）。
    ///
    /// 只在控制器没有缓存设置时调用；返回值会交给 `resolve_theme_setting`（`"light/dark"`
    /// 会按终端明暗解析）。控制器**不会**写回设置管理器。
    pub get_theme_setting: Box<dyn Fn() -> Option<String>>,
    /// TS: `showError(message)`。
    ///
    /// 收到的是**已格式化**的完整消息（含 `Failed to load theme ...` 与回退说明两行）。
    pub show_error: Box<dyn Fn(&str)>,
    /// TS: `onChanged()`。
    ///
    /// 主题应用后调用（成功或失败都调用，因为失败会回退 `system`）；它只做 UI 状态刷新，
    /// 重绘由控制器自己的 `invalidate`/`requestRender` 负责。
    pub on_changed: Box<dyn Fn()>,
    /// TS: `initialThemeSetting`。
    ///
    /// 构造时作为 `current_theme_setting` 的初值；一旦有值就会遮蔽 `get_theme_setting`
    /// （测试 `uses_the_initial_theme_without_persisting_it` 锁定“不写回设置管理器”）。
    pub initial_theme_setting: Option<String>,
}

/// 控制器内部状态（`Rc<RefCell<…>>`：终端的 late reply / 明暗监听器需要同步更新它）。
///
/// 单独包成结构而不是控制器字段，是因为监听器闭包是 `'static` 的：它们持有
/// `Rc<RefCell<ControllerState>>`，在 host 的回调栈里也能更新状态（但拿不到 host）。
struct ControllerState {
    /// 设置读取闭包；只有缓存为空时才调用。
    get_theme_setting: Rc<dyn Fn() -> Option<String>>,
    /// 错误展示回调；只接收最终文案。
    show_error: Rc<dyn Fn(&str)>,
    /// 主题应用后的通知回调；不负责重绘。
    on_changed: Rc<dyn Fn()>,
    /// 缓存的主题设置；`Some` 时优先于 `get_theme_setting`。
    current_theme_setting: Option<String>,
    /// 最近一次应用的终端颜色（多次上报合并后的结果，不是单次上报）。
    terminal_colors: Option<TerminalColors>,
    /// 控制器认为已应用的主题名；`"<in-memory>"` 表示 `set_theme_instance()` 装入的实例。
    active_theme_name: Option<String>,
    /// 明暗方案通知是否开启（关闭时监听器回调直接返回）。
    auto_sync_enabled: bool,
    /// 当前注册的 host 监听器 id（rebind/dispose 时退订）。
    scheme_listener: Option<usize>,
    /// 监听器路径需要补一次 `invalidate()+requestRender()`。
    ui_dirty: bool,
    /// host 提供的共享渲染请求句柄（回调栈里立即 `requestRender()`）。
    render_requested: Option<Rc<std::cell::Cell<bool>>>,
    /// 明暗方案监听器请求了一次新的颜色查询（由 `wait_for_terminal_colors()` 发起）。
    query_requested: bool,
}

/// host 的可选借用（监听器路径没有 host）。
///
/// `Host` 只在方法体执行期间存在；`None` 表示当前处于 host 的回调栈（late reply / 明暗
/// 监听器），此时任何需要 host 的动作都要记进状态，由下一次驱动补做。
enum HostRef<'a> {
    Host(&'a mut dyn ThemeHost),
    None,
}

impl HostRef<'_> {
    /// TS 的 `ui.invalidate(); ui.requestRender();`。
    ///
    /// 在 host 回调路径（late reply / 明暗监听器）里拿不到 host：记下 `ui_dirty`，由下一次
    /// [`flush_ui`] 补上。TS 在这里是立即重绘；见模块文档的「集成缺口」。
    fn invalidate_and_render(&mut self, state: &Rc<RefCell<ControllerState>>) {
        match self {
            HostRef::Host(host) => {
                host.invalidate();
                host.request_render();
            }
            HostRef::None => {
                state.borrow_mut().ui_dirty = true;
                // `requestRender()` 不必等下一次驱动：把宿主的调度标志直接置位（TS 在回调
                // 里同步调用它）。`invalidate()` 只能由下一次 `flush_ui()` 补上。
                let handle = state.borrow().render_requested.clone();
                if let Some(handle) = handle {
                    handle.set(true);
                }
            }
        }
    }
}

fn theme_setting(state: &Rc<RefCell<ControllerState>>) -> Option<String> {
    let (current, getter) = {
        let state = state.borrow();
        (
            state.current_theme_setting.clone(),
            Rc::clone(&state.get_theme_setting),
        )
    };
    // 缓存优先；并且调用 getter 时不持有 `state` 的借用（先克隆 `Rc`），避免 getter 内部
    // 重入控制器时触发 RefCell 双重借用 panic。
    current.or_else(|| getter())
}

/// TS: `resolveThemeName()` —— 没有设置时用 system。
///
/// `resolve_theme_setting()` 对非 auto 且含 `/` 的设置返回 `None`，这里统一回退
/// [`SYSTEM_THEME_NAME`]。
fn resolve_theme_name(state: &Rc<RefCell<ControllerState>>) -> String {
    resolve_theme_setting(theme_setting(state).as_deref(), get_terminal_theme())
        .unwrap_or_else(|| SYSTEM_THEME_NAME.to_string())
}

/// 主题应用后的统一通知：先让 host 失效组件（回调路径则记 `ui_dirty`），再调用 `on_changed`。
///
/// 这里**不**调用 `request_render`：需要重绘的路径（颜色应用）自己调用
/// `invalidate_and_render`。
fn notify_changed(state: &Rc<RefCell<ControllerState>>, host: &mut HostRef<'_>) {
    match host {
        HostRef::Host(host) => host.invalidate(),
        HostRef::None => state.borrow_mut().ui_dirty = true,
    }
    let callback = Rc::clone(&state.borrow().on_changed);
    callback();
}

/// 应用一个具体主题名并同步控制器状态。
///
/// 顺序：
/// 1. `set_theme(name, true)` 换全局主题并启动主题文件 watcher（失败时 `set_theme` 自己
///    回退 `system`）；
/// 2. 记录 `active_theme_name`：成功用原名，失败记 `system`；
/// 3. 让宿主的组件失效并触发 `on_changed`；
/// 4. 只有 `show_error`（设置里显式指定了主题）才把失败文案交给宿主，自动回退保持静默。
fn apply_theme_name(
    state: &Rc<RefCell<ControllerState>>,
    host: &mut HostRef<'_>,
    theme_name: &str,
    show_error: bool,
) -> ThemeResult {
    let result = set_theme(theme_name, true);
    let active_theme_name = if result.success {
        theme_name.to_string()
    } else {
        SYSTEM_THEME_NAME.to_string()
    };
    state.borrow_mut().active_theme_name = Some(active_theme_name);
    notify_changed(state, host);
    if !result.success && show_error {
        let message = format!(
            "Failed to load theme \"{theme_name}\": {}\nFell back to the system theme.",
            result.error.as_deref().unwrap_or_default()
        );
        let show_error = Rc::clone(&state.borrow().show_error);
        show_error(&message);
    }
    result
}

/// TS: `reapplyForTerminal()` —— 终端颜色或外观变化后重新应用设置。
///
/// 触发条件（两者满足其一）：
/// - 解析出的主题名是 `system`：它由终端颜色生成，颜色一变就必须重建；
/// - 解析出的名字与 `active_theme_name` 不同：例如 `light/dark` 设置随明暗切换选边。
///
/// `"<in-memory>"` 实例由用户显式装入，不参与终端的重新应用。这里不展示错误
/// （`show_error = false`），因为名字来自设置而非一次用户操作。
fn reapply_for_terminal(state: &Rc<RefCell<ControllerState>>, host: &mut HostRef<'_>) {
    if state.borrow().active_theme_name.as_deref() == Some("<in-memory>") {
        return;
    }
    let theme_name = resolve_theme_name(state);
    let active_theme_name = state.borrow().active_theme_name.clone();
    // system 无条件重建；其它主题仅在名字变化时重建，避免每次颜色上报都重载文件。
    if theme_name == SYSTEM_THEME_NAME || Some(theme_name.clone()) != active_theme_name {
        apply_theme_name(state, host, &theme_name, false);
    }
}

/// TS: `applyTerminalColors(reported)`。
///
/// `reported` 是**一次**上报（可能是超时时的部分结果）：逐字段与上一次合并——缺的字段保留
/// 旧值，避免超时把已知颜色抹掉。合并结果与旧值完全相同时直接返回（重新渲染会重建全部
/// 组件，代价高）；`palette` 按元素顺序与长度比较。第一次上报（`previous` 为 `None`）即使
/// 是空颜色也会走完整流程，从而清掉 `mark_terminal_colors_pending()` 的灰阶状态。
fn apply_terminal_colors(
    state: &Rc<RefCell<ControllerState>>,
    host: &mut HostRef<'_>,
    reported: TerminalColors,
) {
    let previous = state.borrow().terminal_colors.clone();
    // 逐字段合并：None 表示这次没拿到，保留旧值；palette 整体克隆（索引位置有语义）。
    let next = TerminalColors {
        foreground: reported
            .foreground
            .or_else(|| previous.as_ref().and_then(|colors| colors.foreground)),
        background: reported
            .background
            .or_else(|| previous.as_ref().and_then(|colors| colors.background)),
        palette: reported
            .palette
            .or_else(|| previous.as_ref().and_then(|colors| colors.palette.clone())),
    };
    // 重新渲染会重建所有组件：颜色没变（包括超时）就跳过。
    if let Some(previous) = &previous
        && same_terminal_colors(previous, &next)
    {
        return;
    }
    state.borrow_mut().terminal_colors = Some(next.clone());
    // 写入全局：`get_terminal_theme()` 与 system 主题生成器都从这里读颜色；同时清 pending。
    set_terminal_colors(&next);
    // system 主题要按新颜色重建；`light/dark` 设置可能要换边。
    reapply_for_terminal(state, host);
    host.invalidate_and_render(state);
}

/// TS: `applyTerminalColorSchemeChange(terminalTheme)`（在 host 的监听器回调里运行）。
///
/// 1. auto-sync 关闭时忽略上报（`apply_from_settings` 只在 auto 形式或 `system` 主题时
///    开启它）；
/// 2. 记录上报值，仅当由全部已知信息推导出的终端明暗真的变化时才重新应用主题；
/// 3. 无论是否变化都排一次颜色查询（`query_requested`）：明暗通常伴随调色板变化，TS 在
///    监听器里立即查询，Rust 只能延迟到下一次驱动（见模块文档的「集成缺口」）。
fn apply_terminal_color_scheme_change(
    state: &Rc<RefCell<ControllerState>>,
    terminal_theme: TerminalColorScheme,
) {
    if !state.borrow().auto_sync_enabled {
        return;
    }
    let previous = get_terminal_theme();
    set_terminal_color_scheme(Some(terminal_theme));
    if get_terminal_theme() != previous {
        reapply_for_terminal(state, &mut HostRef::None);
    }
    state.borrow_mut().query_requested = true;
}

/// 若 host 路径留下过 `ui_dirty`，补一次 `invalidate()+requestRender()`。
///
/// 只在能拿到 host 的驱动路径调用；标志先清零，再调用 host。
fn flush_ui(state: &Rc<RefCell<ControllerState>>, host: &mut dyn ThemeHost) {
    let dirty = state.borrow().ui_dirty;
    if dirty {
        state.borrow_mut().ui_dirty = false;
        host.invalidate();
        host.request_render();
    }
}

/// 应用主题设置并让它与终端保持同步。
///
/// TS: `class InteractiveThemeController`。构造时会 `markTerminalColorsPending()` 并
/// `initTheme(activeThemeName, true)`，随后注册明暗方案监听器。
///
/// 控制器不拥有 host（方法按需借用），但持有 host 给的两样东西：监听器 id 与共享渲染
/// 句柄。所有异步动作都需要显式驱动（[`Self::apply_from_settings`] /
/// [`Self::wait_for_terminal_colors`] / [`Self::pump`]）；其中 `apply_from_settings` 在
/// 返回 future 之前就会应用已经就绪的查询结果（见模块文档）。
pub struct InteractiveThemeController {
    state: Rc<RefCell<ControllerState>>,
    /// 尚未完成的颜色查询。
    ///
    /// TS 的 `queryTerminalColors()` 每次都会把 `requestTerminalColors(...).then(apply)` 接到
    /// 自己的 promise 链上：无论调用方是否等待、是否已被更新的查询取代，查询完成时颜色都会
    /// 应用。Rust 的 future 不能在后台自行推进，因此这里保留**所有**未完成查询；下一次
    /// [`Self::wait_for_terminal_colors`] / [`Self::apply_from_settings`] 的 future 被驱动时
    /// 逐个应用（被取代的查询也会补上）。查询在发起时若已就绪则当场应用。
    pending_queries: VecDeque<Pin<Box<dyn Future<Output = TerminalColors>>>>,
}

impl InteractiveThemeController {
    /// TS: `constructor(ui, options)`。
    ///
    /// 构造会立即产生可观察副作用：解析并应用初始主题（`init_theme(..., true)`，同时启动
    /// 主题文件 watcher）、`mark_terminal_colors_pending()`（system 主题先以灰阶渲染）、
    /// 注册明暗方案监听器。此时 `auto_sync_enabled` 仍为 `false`，要等
    /// `apply_from_settings()` 依据设置调用 `set_auto_sync()` 后，host 才开启终端上报。
    ///
    /// `init_theme` 没有返回值：初始主题加载失败时会静默回退 `system`，错误不会经过
    /// `show_error`（与 TS 的构造器一致）。
    pub fn new(host: &mut dyn ThemeHost, options: ThemeControllerOptions) -> Self {
        let state = Rc::new(RefCell::new(ControllerState {
            get_theme_setting: Rc::from(options.get_theme_setting),
            show_error: Rc::from(options.show_error),
            on_changed: Rc::from(options.on_changed),
            current_theme_setting: options.initial_theme_setting,
            terminal_colors: None,
            active_theme_name: None,
            auto_sync_enabled: false,
            scheme_listener: None,
            ui_dirty: false,
            render_requested: None,
            query_requested: false,
        }));
        // 回调栈里立即 requestRender() 的共享句柄（TS 的 ui.requestRender 在监听器里同步生效）。
        let render_requested = host.render_requested_handle();
        state.borrow_mut().render_requested = render_requested;
        // TS: `this.activeThemeName = this.resolveThemeName()`（只解析一次），随后
        // `markTerminalColorsPending()` 与 `initTheme(this.activeThemeName, true)`。
        let active_theme_name = resolve_theme_name(&state);
        state.borrow_mut().active_theme_name = Some(active_theme_name.clone());
        // system 主题先灰阶；颜色到达后再上色。
        mark_terminal_colors_pending();
        init_theme(Some(&active_theme_name), true);
        let controller = InteractiveThemeController {
            state,
            pending_queries: VecDeque::new(),
        };
        controller.bind_terminal_color_scheme_listener(host);
        controller
    }

    /// TS: `rebindTui()` —— 换一个 TUI 实例时重新绑定监听器与通知开关。
    ///
    /// 先退订旧 id（若有），再向新 host 注册新监听器并重新获取渲染句柄；通知开关按当前
    /// `auto_sync_enabled` 重新同步。控制器保留已应用主题与合并后的终端颜色，因此换屏不会
    /// 触发重新查询。
    pub fn rebind_tui(&mut self, host: &mut dyn ThemeHost) {
        let previous = self.state.borrow_mut().scheme_listener.take();
        if let Some(id) = previous {
            host.remove_scheme_listener(id);
        }
        let render_requested = host.render_requested_handle();
        self.state.borrow_mut().render_requested = render_requested;
        let listener_state = Rc::clone(&self.state);
        let id = host.add_scheme_listener(Box::new(move |terminal_theme| {
            apply_terminal_color_scheme_change(&listener_state, terminal_theme);
        }));
        self.state.borrow_mut().scheme_listener = Some(id);
        let auto_sync = self.state.borrow().auto_sync_enabled;
        host.set_scheme_notifications(auto_sync);
    }

    /// TS: `applyFromSettings()`。
    ///
    /// 同步应用主题并**发起**终端颜色查询；返回的 future 等待查询完成并把颜色应用到主题。
    /// 发起时已经就绪的查询当场应用（同步轮询），因此丢弃返回的
    /// future 也不会丢结果；未就绪的查询留在队列里，之后仍可用
    /// [`Self::wait_for_terminal_colors`] 驱动。
    ///
    /// 同步部分的顺序：读设置 → 解析主题名 → 计算并应用 auto-sync → `set_theme` →
    /// 更新 `active_theme_name`／触发 `on_changed` → 发起查询。`show_error` 只在设置里
    /// **显式写了一个主题**（包含 `initial_theme_setting`）时启用；由缺省值解析出的 `system`
    /// 失败只静默回退。
    pub fn apply_from_settings<'a>(
        &'a mut self,
        host: &'a mut dyn ThemeHost,
    ) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
        let theme_setting = theme_setting(&self.state);
        let theme_name = resolve_theme_name(&self.state);
        // auto 形式（`light/dark`）需要跟随终端明暗切换；system 主题本身由终端颜色生成，
        // 同样需要监听变化。固定主题名则关闭通知。
        let auto_sync = parse_auto_theme_setting(theme_setting.as_deref()).is_some()
            || theme_name == SYSTEM_THEME_NAME;
        self.set_auto_sync(host, auto_sync);
        {
            let mut host_ref = HostRef::Host(&mut *host);
            // 只有设置里显式指定了主题时才展示错误。
            apply_theme_name(
                &self.state,
                &mut host_ref,
                &theme_name,
                theme_setting.is_some(),
            );
            self.start_query(&mut host_ref);
        }
        Box::pin(async move {
            self.drive_queued_queries(host).await;
            flush_ui(&self.state, host);
        })
    }

    /// 处理 host 回调排队的动作：发起被请求的颜色查询、补上延后的 `invalidate()+requestRender()`。
    ///
    /// 事件循环应在每次处理完终端输入后调用（见模块文档的「事件循环契约」）；
    /// [`Self::wait_for_terminal_colors`] / [`Self::apply_from_settings`] 也会先处理同一批
    /// 动作。与异步方法不同，`pump()` 不 `await` 任何查询：已就绪的结果当场应用，未就绪的
    /// 留到下一次，因此不会阻塞调用方的事件循环。
    pub fn pump(&mut self, host: &mut dyn ThemeHost) {
        self.begin_deferred(host);
        self.poll_queued_queries(&mut HostRef::Host(host));
    }

    /// 发起明暗监听器排队的查询，并补上 host 回调留下的重绘（不等待查询、不轮询旧查询）。
    ///
    /// 同步执行：调用返回时，`query_requested` 已被消费、查询已交给 host 发起（`TuiBase`
    /// 在调用 `query_terminal_colors_with_late_reply` 时就写出终端查询序列）。
    fn begin_deferred(&mut self, host: &mut dyn ThemeHost) {
        if self.state.borrow().query_requested {
            let mut host_ref = HostRef::Host(&mut *host);
            self.start_query(&mut host_ref);
        }
        flush_ui(&self.state, host);
    }

    /// TS: `waitForTerminalColors()` —— 等**最新**颜色查询完成（或超时）并应用。
    ///
    /// 顺带发起明暗监听器排队的重新查询（TS 在监听器里立即发起；见模块文档的集成缺口），
    /// 以及补上 host 回调路径留下的重绘。与 TS 一样只等待最新查询：被新查询取代的旧查询
    /// 只做非阻塞检查，不会拖慢这次等待。
    ///
    /// 返回的 future 借用 `self` 与 `host`；若创建后立即丢弃，只有同步前缀
    /// （`begin_deferred`：发起排队查询、补 deferred 重绘）生效，队列中的颜色要等下一次驱动。
    pub fn wait_for_terminal_colors<'a>(
        &'a mut self,
        host: &'a mut dyn ThemeHost,
    ) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
        // `begin_deferred` 在返回 future 之前同步发起查询（调用方可以先断言查询已发出）。
        self.begin_deferred(host);
        Box::pin(async move {
            self.drive_queued_queries(host).await;
            flush_ui(&self.state, host);
        })
    }

    /// TS: `getThemeSelection()`。
    ///
    /// 回退链：缓存设置 → `get_theme_setting()` → `active_theme_name`。第三项在构造时已解析
    /// （通常是 `system`），因此正常使用中不会返回 `None`。
    pub fn get_theme_selection(&self) -> Option<String> {
        let current = self.state.borrow().current_theme_setting.clone();
        if current.is_some() {
            return current;
        }
        let setting = {
            let getter = Rc::clone(&self.state.borrow().get_theme_setting);
            getter()
        };
        setting.or_else(|| self.state.borrow().active_theme_name.clone())
    }

    /// TS: `setThemeName(themeName, showError = false)`。
    ///
    /// 先按 `themeName == "system"` 切换 auto-sync，再应用主题。**只有成功时**才把名字写入
    /// `current_theme_setting` 缓存；失败时缓存保持原值（全局主题已由 `set_theme` 回退
    /// `system`），因此下一次 `apply_from_settings()` 仍按缓存或 getter 的旧值解析。
    /// `show_error` 控制是否把失败文案交给宿主。
    pub fn set_theme_name(
        &mut self,
        host: &mut dyn ThemeHost,
        theme_name: &str,
        show_error: bool,
    ) -> ThemeResult {
        self.set_auto_sync(host, theme_name == SYSTEM_THEME_NAME);
        let result = {
            let mut host_ref = HostRef::Host(host);
            apply_theme_name(&self.state, &mut host_ref, theme_name, show_error)
        };
        if result.success {
            self.state.borrow_mut().current_theme_setting = Some(theme_name.to_string());
        }
        result
    }

    /// TS: `setThemeSetting(themeSetting)`。
    ///
    /// 先把新设置写进缓存（覆盖 `current_theme_setting`），再走 `apply_from_settings()`。
    /// 因为读设置缓存优先，从这次调用起 `get_theme_setting()` 的返回值不再影响本控制器。
    pub fn set_theme_setting<'a>(
        &'a mut self,
        host: &'a mut dyn ThemeHost,
        theme_setting: &str,
    ) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
        self.state.borrow_mut().current_theme_setting = Some(theme_setting.to_string());
        self.apply_from_settings(host)
    }

    /// TS: `setThemeInstance(themeInstance)`。
    ///
    /// 关闭 auto-sync，装入内存主题，并把 `active_theme_name` 记为 `"<in-memory>"`；此后
    /// 终端颜色/明暗变化触发的 `reapply_for_terminal()` 会直接返回，不会覆盖该实例。
    /// 总是返回成功（`set_theme_instance` 本身没有失败路径）。
    pub fn set_theme_instance(
        &mut self,
        host: &mut dyn ThemeHost,
        theme_instance: std::sync::Arc<Theme>,
    ) -> ThemeResult {
        self.set_auto_sync(host, false);
        set_theme_instance(theme_instance);
        self.state.borrow_mut().active_theme_name = Some("<in-memory>".to_string());
        notify_changed(&self.state, &mut HostRef::Host(host));
        ThemeResult::ok()
    }

    /// TS: `preview(themeSettingOrName)`。
    ///
    /// 只切换**全局**主题用于预览：成功时立即 `invalidate()+requestRender()`，但不更新
    /// 控制器的 `active_theme_name` / `current_theme_setting`（之后
    /// `apply_from_settings()` 会按设置重新应用）。失败时 `set_theme` 已把全局主题回退
    /// `system`，这里不展示错误也不重绘。
    pub fn preview(&mut self, host: &mut dyn ThemeHost, theme_setting_or_name: &str) {
        let theme_name = resolve_theme_setting(Some(theme_setting_or_name), get_terminal_theme())
            .or_else(|| self.state.borrow().active_theme_name.clone());
        let Some(theme_name) = theme_name else {
            return;
        };
        if set_theme(&theme_name, true).success {
            host.invalidate();
            host.request_render();
        }
    }

    /// TS: `disableAutoSync()`。
    ///
    /// 只关闭终端明暗通知（host 的 `set_scheme_notifications(false)`），不注销监听器。
    pub fn disable_auto_sync(&mut self, host: &mut dyn ThemeHost) {
        self.set_auto_sync(host, false);
    }

    /// TS: `dispose()`。
    ///
    /// 关闭通知并退订监听器；可重复调用（第二次没有 id 可退）。不清理全局主题。
    ///
    /// 若不调用：host 仍持有监听器闭包（闭包共享 `ControllerState`）；只要 host 还派发
    /// 明暗上报，闭包就会继续更新全局终端明暗并可能重新应用主题，直到 host 本身被销毁。
    pub fn dispose(&mut self, host: &mut dyn ThemeHost) {
        self.set_auto_sync(host, false);
        let listener = self.state.borrow_mut().scheme_listener.take();
        if let Some(id) = listener {
            host.remove_scheme_listener(id);
        }
    }

    /// TS: `getTerminalTheme()`。
    ///
    /// 读取全局推导值（`theme::get_terminal_theme()`），与控制器缓存的
    /// `terminal_colors` 不是同一份数据：在首次颜色应用之前，全局值可能已由启动路径写入。
    pub fn get_terminal_theme(&self) -> TerminalColorScheme {
        get_terminal_theme()
    }

    /// 注册明暗方案监听器；闭包捕获共享状态而不是 `&mut self`，因此能在 host 回调栈里运行。
    fn bind_terminal_color_scheme_listener(&self, host: &mut dyn ThemeHost) {
        let listener_state = Rc::clone(&self.state);
        let id = host.add_scheme_listener(Box::new(move |terminal_theme| {
            apply_terminal_color_scheme_change(&listener_state, terminal_theme);
        }));
        self.state.borrow_mut().scheme_listener = Some(id);
    }

    /// 发起一次颜色查询（TS: `this.terminalColorQuery = requestTerminalColors(...)`）。
    ///
    /// 在 tokio 运行时上下文里先轮询一次：已经就绪就当场应用（TS 的 `.then(apply)` 在
    /// 微任务里执行，调用方不等待也会应用）。未就绪（或不在运行时里）的 future 进入队列。
    ///
    /// 两个细节：
    /// - 轮询用 `Waker::noop()`：控制器不依赖唤醒，进度由调用方的下一次驱动推进；
    /// - late reply 回调里没有 host，因此颜色应用走 `HostRef::None`（重绘延后到下一次驱动）。
    ///
    /// 调用会清掉 `query_requested`（不管 future 是否就绪），所以一次驱动不会重复发起查询。
    fn start_query(&mut self, host: &mut HostRef<'_>) {
        let state = Rc::clone(&self.state);
        let late: Box<dyn FnMut(TerminalColors)> = Box::new(move |colors| {
            apply_terminal_colors(&state, &mut HostRef::None, colors);
        });
        let mut query = match host {
            HostRef::Host(host) => {
                host.query_terminal_colors(TERMINAL_QUERY_TIMEOUT_MS, Some(late))
            }
            HostRef::None => Box::pin(async { TerminalColors::default() }),
        };
        self.state.borrow_mut().query_requested = false;
        // `TuiBase` 的查询 future 依赖 tokio 定时器，只能在运行时上下文里 poll。
        if tokio::runtime::Handle::try_current().is_ok() {
            let mut context = Context::from_waker(Waker::noop());
            if let Poll::Ready(colors) = query.as_mut().poll(&mut context) {
                apply_terminal_colors(&self.state, host, colors);
                return;
            }
        }
        self.pending_queries.push_back(query);
    }

    /// 非阻塞地推进队列里的查询：就绪的应用，未就绪的留到下一次驱动。
    ///
    /// 旧查询可能永远不会结束（被取代的 future 在宿主侧仍会在超时后完成；测试的假实现
    /// 则可能一直挂着），所以不能用 `await` 等它们。
    ///
    /// 没有 tokio 运行时时直接返回、队列原样保留；每次轮询都用新的 `Waker::noop()`，
    /// `Pending` 的 future 不会被唤醒，只能等下一次显式驱动。
    fn poll_queued_queries(&mut self, host: &mut HostRef<'_>) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let mut pending = VecDeque::with_capacity(self.pending_queries.len());
        while let Some(mut query) = self.pending_queries.pop_front() {
            let mut context = Context::from_waker(Waker::noop());
            match query.as_mut().poll(&mut context) {
                Poll::Ready(colors) => apply_terminal_colors(&self.state, host, colors),
                Poll::Pending => pending.push_back(query),
            }
        }
        self.pending_queries = pending;
    }

    /// 驱动查询队列（TS 的 `waitForTerminalColors()` 语义）。
    ///
    /// 与 TS 一样只**等待最新**一次查询（旧查询各自在自己的 promise 链上应用）；旧查询在
    /// 驱动前后各做一次非阻塞检查，已就绪的结果补上。
    ///
    /// 顺序：取出队尾（最新）→ 轮询其余旧查询 → `await` 最新查询并应用 → 再轮询一次
    /// （等待期间旧查询可能已经完成）。`await` 期间 `host` 被这个 future 独占借用，因此
    /// 期间不会发生新的驱动。
    async fn drive_queued_queries(&mut self, host: &mut dyn ThemeHost) {
        let latest = self.pending_queries.pop_back();
        self.poll_queued_queries(&mut HostRef::Host(host));
        if let Some(latest) = latest {
            let colors = latest.await;
            apply_terminal_colors(&self.state, &mut HostRef::Host(host), colors);
        }
        self.poll_queued_queries(&mut HostRef::Host(host));
    }

    /// 切换 auto-sync 并同步 host 的通知开关；值没变时直接返回，不调用 host。
    fn set_auto_sync(&mut self, host: &mut dyn ThemeHost, enabled: bool) {
        if self.state.borrow().auto_sync_enabled == enabled {
            return;
        }
        self.state.borrow_mut().auto_sync_enabled = enabled;
        host.set_scheme_notifications(enabled);
    }
}

// =============================================================================
// kiana-tui adapters
// =============================================================================

// 两个适配实现只做转发，映射关系：
// - query_terminal_colors → TuiBase::query_terminal_colors_with_late_reply（它返回不借用
//   TuiBase 的 'static future，恰好满足 ThemeHost 的存储要求）；
// - invalidate → invalidate_all（TS `ui.invalidate()` 的语义是所有根与 overlay 组件失效）；
// - request_render → request_render(false)（**非强制**：force=true 会先
//   `reset_render_state()` 再请求立即渲染，见 `TuiBase::request_render`；控制器只需要
//   “请求一帧”）。

/// `TuiBase` 的直接适配。
impl ThemeHost for TuiBase {
    fn query_terminal_colors(
        &mut self,
        timeout_ms: u64,
        on_late_reply: Option<Box<dyn FnMut(TerminalColors)>>,
    ) -> Pin<Box<dyn Future<Output = TerminalColors> + 'static>> {
        TuiBase::query_terminal_colors_with_late_reply(self, timeout_ms, on_late_reply)
    }

    fn set_scheme_notifications(&mut self, enabled: bool) {
        TuiBase::set_terminal_color_scheme_notifications(self, enabled);
    }

    fn add_scheme_listener(&mut self, listener: Box<dyn FnMut(TerminalColorScheme)>) -> usize {
        TuiBase::on_terminal_color_scheme_change(self, listener)
    }

    fn remove_scheme_listener(&mut self, id: usize) {
        TuiBase::remove_terminal_color_scheme_listener(self, id);
    }

    fn invalidate(&mut self) {
        TuiBase::invalidate_all(self);
    }

    fn request_render(&mut self) {
        TuiBase::request_render(self, false);
    }

    fn render_requested_handle(&self) -> Option<Rc<std::cell::Cell<bool>>> {
        Some(TuiBase::render_requested_handle(self))
    }
}

/// `TuiMainScreen` 的适配：所有调用转发到内部 `TuiBase`（`base_mut()`/`base()`）。
impl ThemeHost for kiana_tui::TuiMainScreen {
    fn query_terminal_colors(
        &mut self,
        timeout_ms: u64,
        on_late_reply: Option<Box<dyn FnMut(TerminalColors)>>,
    ) -> Pin<Box<dyn Future<Output = TerminalColors> + 'static>> {
        self.base_mut()
            .query_terminal_colors_with_late_reply(timeout_ms, on_late_reply)
    }

    fn set_scheme_notifications(&mut self, enabled: bool) {
        self.base_mut()
            .set_terminal_color_scheme_notifications(enabled);
    }

    fn add_scheme_listener(&mut self, listener: Box<dyn FnMut(TerminalColorScheme)>) -> usize {
        self.base_mut().on_terminal_color_scheme_change(listener)
    }

    fn remove_scheme_listener(&mut self, id: usize) {
        self.base_mut().remove_terminal_color_scheme_listener(id);
    }

    fn invalidate(&mut self) {
        self.base_mut().invalidate_all();
    }

    fn request_render(&mut self) {
        self.base_mut().request_render(false);
    }

    fn render_requested_handle(&self) -> Option<Rc<std::cell::Cell<bool>>> {
        Some(self.base().render_requested_handle())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_and_terminal_color_comparisons() {
        let rgb = |r, g, b| RgbColor { r, g, b };
        assert!(same_rgb(None, None));
        assert!(same_rgb(Some(rgb(1, 2, 3)), Some(rgb(1, 2, 3))));
        assert!(!same_rgb(Some(rgb(1, 2, 3)), Some(rgb(1, 2, 4))));
        assert!(!same_rgb(Some(rgb(1, 2, 3)), None));

        let empty = TerminalColors::default();
        assert!(same_terminal_colors(&empty, &TerminalColors::default()));
        let with_palette = TerminalColors {
            palette: Some(vec![rgb(1, 2, 3)]),
            ..TerminalColors::default()
        };
        assert!(same_terminal_colors(&with_palette, &with_palette.clone()));
        assert!(!same_terminal_colors(&empty, &with_palette));
        assert!(!same_terminal_colors(
            &with_palette,
            &TerminalColors {
                palette: Some(vec![rgb(1, 2, 3), rgb(4, 5, 6)]),
                ..TerminalColors::default()
            }
        ));
    }

    #[test]
    fn request_terminal_colors_applies_the_result_and_late_replies() {
        use std::sync::mpsc;

        struct FakeHost {
            late: Option<Box<dyn FnMut(TerminalColors)>>,
        }
        impl ThemeHost for FakeHost {
            fn query_terminal_colors(
                &mut self,
                _timeout_ms: u64,
                on_late_reply: Option<Box<dyn FnMut(TerminalColors)>>,
            ) -> Pin<Box<dyn Future<Output = TerminalColors> + 'static>> {
                self.late = on_late_reply;
                Box::pin(async {
                    TerminalColors {
                        foreground: Some(RgbColor { r: 1, g: 2, b: 3 }),
                        ..TerminalColors::default()
                    }
                })
            }
            fn set_scheme_notifications(&mut self, _enabled: bool) {}
            fn add_scheme_listener(
                &mut self,
                _listener: Box<dyn FnMut(TerminalColorScheme)>,
            ) -> usize {
                1
            }
            fn remove_scheme_listener(&mut self, _id: usize) {}
            fn invalidate(&mut self) {}
            fn request_render(&mut self) {}
        }

        let mut host = FakeHost { late: None };
        let (sender, receiver) = mpsc::channel::<TerminalColors>();
        let future = request_terminal_colors(
            &mut host,
            Box::new(move |colors| {
                let _ = sender.send(colors);
            }),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(future);
        let applied = receiver.recv().expect("query result applied");
        assert_eq!(applied.foreground, Some(RgbColor { r: 1, g: 2, b: 3 }));

        // 迟到的回复也走同一个 apply。
        let late = host.late.as_mut().expect("late reply handler");
        late(TerminalColors {
            background: Some(RgbColor { r: 9, g: 9, b: 9 }),
            ..TerminalColors::default()
        });
        let applied = receiver.recv().expect("late reply applied");
        assert_eq!(applied.background, Some(RgbColor { r: 9, g: 9, b: 9 }));
    }
}
