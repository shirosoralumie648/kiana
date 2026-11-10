//! `theme-controller.test.ts` 的逐条移植。
//!
//! TS 的 `TUI` 假实现换成了 [`FakeHost`]（[`ThemeHost`]），`SettingsManager.inMemory` 换成
//! 一个只读的 `get_theme_setting` 闭包。`await flush()` 对应
//! `controller.wait_for_terminal_colors(&mut host).await`（见 `theme_controller` 模块文档）。

mod common;

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use common::{EnvGuard, lock, reset_theme_state};
use kiana_theme::{
    InteractiveThemeController, THEME, ThemeColor, ThemeControllerOptions, ThemeHost, ThemeResult,
    ThemeStyle, ThemeToken, theme,
};
use kiana_tui::{Color, RgbColor, TerminalColorScheme, TerminalColors};

fn rgb(r: u8, g: u8, b: u8) -> RgbColor {
    RgbColor { r, g, b }
}

fn dark_colors() -> TerminalColors {
    TerminalColors {
        foreground: Some(rgb(248, 248, 242)),
        background: Some(rgb(40, 42, 54)),
        ..TerminalColors::default()
    }
}

fn light_colors() -> TerminalColors {
    TerminalColors {
        foreground: Some(rgb(30, 30, 30)),
        background: Some(rgb(250, 250, 250)),
        ..TerminalColors::default()
    }
}

#[derive(Default)]
struct FakeState {
    /// `mockResolvedValue`：之后每次查询都返回它。
    resolved: TerminalColors,
    /// `mockReturnValue(new Promise(...))`：查询挂起，等待 [`FakeHost::answer`]。
    gated: bool,
    gate: Option<tokio::sync::oneshot::Sender<TerminalColors>>,
    late_reply: Option<Box<dyn FnMut(TerminalColors)>>,
    query_count: usize,
    notifications: Vec<bool>,
    unsubscribe_count: usize,
    listener: Option<Box<dyn FnMut(TerminalColorScheme)>>,
    invalidate_count: usize,
    render_count: usize,
}

/// TS 测试里的 `createUi()`。
struct FakeHost {
    state: Rc<RefCell<FakeState>>,
    /// `TuiBase::render_requested_handle()` 的等价物：回调栈里也能立即置位的调度标志。
    render_requested: Rc<std::cell::Cell<bool>>,
}

impl FakeHost {
    fn new() -> Self {
        FakeHost {
            state: Rc::new(RefCell::new(FakeState::default())),
            render_requested: Rc::new(std::cell::Cell::new(false)),
        }
    }

    fn set_result(&self, colors: TerminalColors) {
        self.state.borrow_mut().resolved = colors;
    }

    fn set_gated(&self, gated: bool) {
        self.state.borrow_mut().gated = gated;
    }

    /// 解析挂起的查询（TS 的 `answer(DARK)`）。
    fn answer(&self, colors: TerminalColors) {
        if let Some(sender) = self.state.borrow_mut().gate.take() {
            let _ = sender.send(colors);
        }
    }

    /// 触发终端的 late reply（TS: 调用保存下来的 `onLateReply`）。
    fn emit_late_reply(&self, colors: TerminalColors) {
        let mut state = self.state.borrow_mut();
        if let Some(listener) = state.late_reply.as_mut() {
            listener(colors);
        }
    }

    /// 触发明暗方案监听器（TS 的 `emitTerminalColorScheme`）。
    fn emit_terminal_color_scheme(&self, scheme: TerminalColorScheme) {
        let mut state = self.state.borrow_mut();
        if let Some(listener) = state.listener.as_mut() {
            listener(scheme);
        }
    }

    fn query_count(&self) -> usize {
        self.state.borrow().query_count
    }

    fn notifications(&self) -> Vec<bool> {
        self.state.borrow().notifications.clone()
    }

    fn unsubscribe_count(&self) -> usize {
        self.state.borrow().unsubscribe_count
    }

    fn render_count(&self) -> usize {
        self.state.borrow().render_count
    }

    fn invalidate_count(&self) -> usize {
        self.state.borrow().invalidate_count
    }

    /// 宿主的共享渲染标志（`ThemeHost::render_requested_handle`）。
    fn render_requested(&self) -> bool {
        self.render_requested.get()
    }

    fn take_render_request(&self) -> bool {
        self.render_requested.replace(false)
    }
}

impl ThemeHost for FakeHost {
    fn query_terminal_colors(
        &mut self,
        _timeout_ms: u64,
        on_late_reply: Option<Box<dyn FnMut(TerminalColors)>>,
    ) -> Pin<Box<dyn Future<Output = TerminalColors> + 'static>> {
        let mut state = self.state.borrow_mut();
        state.query_count += 1;
        state.late_reply = on_late_reply;
        if state.gated {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            state.gate = Some(sender);
            Box::pin(async move { receiver.await.unwrap_or_default() })
        } else {
            let colors = state.resolved.clone();
            Box::pin(async move { colors })
        }
    }

    fn set_scheme_notifications(&mut self, enabled: bool) {
        self.state.borrow_mut().notifications.push(enabled);
    }

    fn add_scheme_listener(&mut self, listener: Box<dyn FnMut(TerminalColorScheme)>) -> usize {
        let mut state = self.state.borrow_mut();
        state.listener = Some(listener);
        1
    }

    fn remove_scheme_listener(&mut self, _id: usize) {
        let mut state = self.state.borrow_mut();
        state.unsubscribe_count += 1;
        state.listener = None;
    }

    fn invalidate(&mut self) {
        self.state.borrow_mut().invalidate_count += 1;
    }

    fn request_render(&mut self) {
        self.state.borrow_mut().render_count += 1;
    }

    fn render_requested_handle(&self) -> Option<Rc<std::cell::Cell<bool>>> {
        Some(Rc::clone(&self.render_requested))
    }
}

#[derive(Default)]
struct Signals {
    errors: Vec<String>,
    changed: usize,
}

fn create_controller(
    host: &mut FakeHost,
    get_theme_setting: impl Fn() -> Option<String> + 'static,
    initial_theme_setting: Option<&str>,
) -> (InteractiveThemeController, Rc<RefCell<Signals>>) {
    let signals = Rc::new(RefCell::new(Signals::default()));
    let error_signals = Rc::clone(&signals);
    let change_signals = Rc::clone(&signals);
    let controller = InteractiveThemeController::new(
        host,
        ThemeControllerOptions {
            get_theme_setting: Box::new(get_theme_setting),
            show_error: Box::new(move |message: &str| {
                error_signals.borrow_mut().errors.push(message.to_string());
            }),
            on_changed: Box::new(move || {
                change_signals.borrow_mut().changed += 1;
            }),
            initial_theme_setting: initial_theme_setting.map(str::to_string),
        },
    );
    (controller, signals)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

fn constant_setting(value: Option<&'static str>) -> impl Fn() -> Option<String> {
    move || value.map(str::to_string)
}

fn theme_name() -> Option<String> {
    theme().name().expect("theme initialized")
}

#[test]
fn uses_the_initial_theme_without_persisting_it() {
    let _lock = lock();
    reset_theme_state();
    let runtime = runtime();
    let mut host = FakeHost::new();
    let manager = Rc::new(RefCell::new(Some("dark".to_string())));
    let getter = {
        let manager = Rc::clone(&manager);
        move || manager.borrow().clone()
    };
    let (mut controller, _signals) = create_controller(&mut host, getter, Some("light"));

    assert_eq!(theme_name().as_deref(), Some("light"));
    assert_eq!(controller.get_theme_selection().as_deref(), Some("light"));
    runtime.block_on(controller.apply_from_settings(&mut host));

    assert_eq!(host.query_count(), 1);
    // `setTheme`/`flush` 没有被调用：Rust 侧根本没有持久化 API，设置值原样保留。
    assert_eq!(manager.borrow().as_deref(), Some("dark"));
}

#[test]
fn applies_the_theme_immediately_and_lets_startup_wait_for_the_colors() {
    let _lock = lock();
    reset_theme_state();
    let runtime = runtime();
    let mut host = FakeHost::new();
    host.set_gated(true);
    let (mut controller, _signals) = create_controller(&mut host, constant_setting(None), None);
    drop(controller.apply_from_settings(&mut host));

    // 终端回色之前是灰阶。
    assert_eq!(theme_name().as_deref(), Some("system"));
    assert_eq!(
        THEME
            .get_fg_ansi(ThemeColor::Error)
            .expect("theme initialized"),
        "\x1b[39m"
    );

    host.answer(dark_colors());
    runtime.block_on(controller.wait_for_terminal_colors(&mut host));
    assert!(
        THEME
            .get_fg_ansi(ThemeColor::Error)
            .expect("theme initialized")
            .starts_with("\x1b[38;")
    );
}

#[test]
fn falls_back_to_palette_indices_then_applies_colors_that_arrive_after_the_timeout() {
    let _lock = lock();
    reset_theme_state();
    let runtime = runtime();
    let mut host = FakeHost::new();
    let (mut controller, _signals) = create_controller(&mut host, constant_setting(None), None);
    drop(controller.apply_from_settings(&mut host));
    runtime.block_on(controller.wait_for_terminal_colors(&mut host));
    assert_eq!(
        THEME
            .get_fg_ansi(ThemeColor::Error)
            .expect("theme initialized"),
        "\x1b[38;5;1m"
    );

    host.emit_late_reply(dark_colors());
    assert!(matches!(
        THEME.color(ThemeToken::Error).expect("theme initialized"),
        Some(Color::Rgb { .. })
    ));
}

#[test]
fn re_queries_the_colors_on_appearance_changes_and_lets_them_decide() {
    let _lock = lock();
    reset_theme_state();
    let runtime = runtime();
    let mut host = FakeHost::new();
    host.set_result(light_colors());
    let (mut controller, _signals) =
        create_controller(&mut host, constant_setting(None), Some("light/dark"));
    drop(controller.apply_from_settings(&mut host));
    assert_eq!(host.notifications(), vec![true]);
    runtime.block_on(controller.wait_for_terminal_colors(&mut host));
    assert_eq!(theme_name().as_deref(), Some("light"));

    host.set_result(dark_colors());
    // The report says light, but the terminal renders dark.
    host.emit_terminal_color_scheme(TerminalColorScheme::Light);
    runtime.block_on(controller.wait_for_terminal_colors(&mut host));
    assert_eq!(theme_name().as_deref(), Some("dark"));
}

#[test]
fn uses_the_reported_scheme_for_the_system_theme_when_the_terminal_reports_no_colors() {
    let _lock = lock();
    reset_theme_state();
    let _env = EnvGuard::set("COLORFGBG", "");
    let runtime = runtime();
    let mut host = FakeHost::new();
    let (mut controller, _signals) = create_controller(&mut host, constant_setting(None), None);
    drop(controller.apply_from_settings(&mut host));
    runtime.block_on(controller.wait_for_terminal_colors(&mut host));
    assert_eq!(
        theme().appearance().expect("theme initialized"),
        TerminalColorScheme::Dark
    );

    host.emit_terminal_color_scheme(TerminalColorScheme::Light);
    assert_eq!(
        theme().appearance().expect("theme initialized"),
        TerminalColorScheme::Light
    );
    assert_eq!(controller.get_terminal_theme(), TerminalColorScheme::Light);
}

#[test]
fn re_renders_only_when_the_reported_colors_change() {
    let _lock = lock();
    reset_theme_state();
    let runtime = runtime();
    let mut host = FakeHost::new();
    let (mut controller, _signals) =
        create_controller(&mut host, constant_setting(Some("dark")), None);

    let query = |colors: TerminalColors,
                 controller: &mut InteractiveThemeController,
                 host: &mut FakeHost| {
        host.set_result(colors);
        drop(controller.apply_from_settings(host));
        runtime.block_on(controller.wait_for_terminal_colors(host));
    };
    query(dark_colors(), &mut controller, &mut host);
    // 超时保留已知颜色；把颜色抹掉会被当成变化并重绘。
    query(TerminalColors::default(), &mut controller, &mut host);
    query(dark_colors(), &mut controller, &mut host);
    assert_eq!(host.render_count(), 1);
    // `notifyChanged()` 每次应用主题都会失效组件（不需要重绘）。
    assert!(host.invalidate_count() >= 3);
}

#[test]
fn disables_terminal_appearance_updates_when_disposed() {
    let _lock = lock();
    reset_theme_state();
    let runtime = runtime();
    let mut host = FakeHost::new();
    let (mut controller, _signals) =
        create_controller(&mut host, constant_setting(Some("light/dark")), None);
    drop(controller.apply_from_settings(&mut host));
    runtime.block_on(controller.wait_for_terminal_colors(&mut host));

    controller.dispose(&mut host);

    assert_eq!(host.notifications().last().copied(), Some(false));
    assert_eq!(host.unsubscribe_count(), 1);
}

#[test]
fn lets_an_explicit_selection_replace_the_initial_theme() {
    let _lock = lock();
    reset_theme_state();
    let runtime = runtime();
    let mut host = FakeHost::new();
    let setting = Rc::new(RefCell::new(Some("dark".to_string())));
    let getter = {
        let setting = Rc::clone(&setting);
        move || setting.borrow().clone()
    };
    let (mut controller, signals) = create_controller(&mut host, getter, Some("light"));
    drop(controller.apply_from_settings(&mut host));

    assert_eq!(
        controller.set_theme_name(&mut host, "dark", false),
        ThemeResult::ok()
    );
    *setting.borrow_mut() = Some("light".to_string());
    drop(controller.apply_from_settings(&mut host));
    runtime.block_on(controller.wait_for_terminal_colors(&mut host));

    assert_eq!(controller.get_theme_selection().as_deref(), Some("dark"));
    assert_eq!(theme_name().as_deref(), Some("dark"));
    assert!(signals.borrow().changed >= 2);
}

#[test]
fn reloads_theme_settings_when_no_initial_theme_was_supplied() {
    let _lock = lock();
    reset_theme_state();
    let mut host = FakeHost::new();
    let setting = Rc::new(RefCell::new(Some("dark".to_string())));
    let getter = {
        let setting = Rc::clone(&setting);
        move || setting.borrow().clone()
    };
    let (mut controller, _signals) = create_controller(&mut host, getter, None);
    drop(controller.apply_from_settings(&mut host));

    *setting.borrow_mut() = Some("light".to_string());
    drop(controller.apply_from_settings(&mut host));
    assert_eq!(theme_name().as_deref(), Some("light"));

    *setting.borrow_mut() = Some("dark".to_string());
    drop(controller.apply_from_settings(&mut host));
    assert_eq!(theme_name().as_deref(), Some("dark"));
}

/// 额外覆盖：`preview()` / `set_theme_instance()` / `disableAutoSync()` 的错误与状态。
#[test]
fn preview_and_in_memory_instances_keep_the_controller_in_sync() {
    let _lock = lock();
    reset_theme_state();
    let mut host = FakeHost::new();
    let (mut controller, _signals) =
        create_controller(&mut host, constant_setting(None), Some("dark/solar"));
    drop(controller.apply_from_settings(&mut host));

    // preview 在主题存在时切换全局主题并重绘。
    let renders_before = host.render_count();
    controller.preview(&mut host, "light");
    assert_eq!(theme_name().as_deref(), Some("light"));
    assert!(host.render_count() > renders_before);

    // 无效主题名不重绘；`setTheme` 失败后回退到 system（与 TS 的 setTheme 一致）。
    let renders_before = host.render_count();
    controller.preview(&mut host, "not-a-theme");
    assert_eq!(theme_name().as_deref(), Some("system"));
    assert_eq!(host.render_count(), renders_before);

    // in-memory 主题跳过后续终端的重新应用。
    let instance = kiana_theme::get_theme_by_name("dark").expect("dark theme");
    assert_eq!(
        controller.set_theme_instance(&mut host, instance),
        ThemeResult::ok()
    );
    assert_eq!(theme_name().as_deref(), Some("dark"));

    controller.disable_auto_sync(&mut host);
    assert_eq!(host.notifications().last().copied(), Some(false));
}

/// 额外覆盖：显式设置里的错误会通过 `show_error` 冒出来（逐字文案）。
#[test]
fn reports_failed_explicit_themes_with_the_fallback_message() {
    let _lock = lock();
    reset_theme_state();
    let mut host = FakeHost::new();
    let (mut controller, signals) = create_controller(&mut host, constant_setting(None), None);
    let result = controller.set_theme_name(&mut host, "does-not-exist", true);
    assert!(!result.success);
    assert_eq!(
        signals.borrow().errors.last().map(String::as_str),
        Some(
            "Failed to load theme \"does-not-exist\": Theme not found: does-not-exist\nFell back to the system theme."
        )
    );
    assert_eq!(theme_name().as_deref(), Some("system"));
}

/// 额外覆盖：`ThemeStyle` 的构建器与 `--` 之外的属性组合。
#[test]
fn theme_style_builders_compose_attributes() {
    let _lock = lock();
    reset_theme_state();
    let style = ThemeStyle::new()
        .fg(ThemeColor::Success)
        .bg(kiana_theme::ThemeBg::ToolSuccessBg)
        .bold()
        .italic();
    assert_eq!(style.attributes.bold, Some(true));
    assert_eq!(style.attributes.italic, Some(true));
    let rendered = THEME.style("x", &style).expect("style");
    assert!(rendered.starts_with('\u{1b}'));
}

/// 返回的 future 被丢弃时，已就绪的查询仍然当场应用（TS 的 `promise.then(apply)`）。
#[test]
fn applies_ready_color_queries_even_when_the_returned_future_is_dropped() {
    let _lock = lock();
    reset_theme_state();
    let runtime = runtime();
    let mut host = FakeHost::new();
    host.set_result(dark_colors());
    let (mut controller, _signals) = create_controller(&mut host, constant_setting(None), None);

    runtime.block_on(async {
        // 直接丢弃：不等待 wait_for_terminal_colors。
        drop(controller.apply_from_settings(&mut host));
    });

    assert_eq!(host.query_count(), 1);
    assert!(matches!(
        THEME.color(ThemeToken::Error).expect("theme initialized"),
        Some(Color::Rgb { .. })
    ));
}

/// 被新查询取代的旧查询：下一次驱动队列时结果仍会应用（TS 的 promise 链各自 apply）。
#[test]
fn applies_superseded_query_results_when_they_are_later_driven() {
    let _lock = lock();
    reset_theme_state();
    let runtime = runtime();
    let mut host = FakeHost::new();
    host.set_gated(true);
    let (mut controller, _signals) = create_controller(&mut host, constant_setting(None), None);
    drop(controller.apply_from_settings(&mut host));
    host.set_gated(false);
    host.set_result(light_colors());
    drop(controller.apply_from_settings(&mut host));
    assert_eq!(host.query_count(), 2);

    // 第二次查询已就绪：wait 只等待最新查询（与 TS 的 `waitForTerminalColors()` 相同），
    // 旧查询只做一次非阻塞检查（仍然未就绪，留在队列里等下一次驱动）。
    runtime.block_on(controller.wait_for_terminal_colors(&mut host));
    assert!(matches!(
        THEME.color(ThemeToken::Error).expect("theme initialized"),
        Some(Color::Rgb { .. })
    ));
    assert!(host.invalidate_count() > 0);
}

/// host 回调路径（late reply / 明暗监听器）留下的重绘在下一次驱动时补上。
#[test]
fn flushes_repaints_deferred_from_host_callbacks() {
    let _lock = lock();
    reset_theme_state();
    let runtime = runtime();
    let mut host = FakeHost::new();
    host.set_result(TerminalColors::default());
    let (mut controller, _signals) = create_controller(&mut host, constant_setting(None), None);
    runtime.block_on(async {
        drop(controller.apply_from_settings(&mut host));
    });

    host.emit_late_reply(dark_colors());
    let invalidates = host.invalidate_count();
    let renders = host.render_count();
    runtime.block_on(controller.wait_for_terminal_colors(&mut host));
    assert!(host.invalidate_count() > invalidates);
    assert!(host.render_count() > renders);
}

/// 回归（review finding 1）：`applyFromSettings()` 返回的 future 单独驱动即可完成启动配色流程
/// —— 不需要额外的 `waitForTerminalColors()`。
#[test]
fn applies_startup_colors_from_apply_from_settings_alone() {
    let _lock = lock();
    reset_theme_state();
    let runtime = runtime();
    let mut host = FakeHost::new();
    host.set_gated(true);
    let (mut controller, _signals) = create_controller(&mut host, constant_setting(None), None);

    let gate = Rc::clone(&host.state);
    let future = controller.apply_from_settings(&mut host);
    // 查询仍在途：此刻 `host` 被 future 借用，通过共享状态投递结果（等价于 `answer()`）。
    if let Some(sender) = gate.borrow_mut().gate.take() {
        let _ = sender.send(dark_colors());
    }
    runtime.block_on(future);

    assert!(matches!(
        THEME.color(ThemeToken::Error).expect("theme initialized"),
        Some(Color::Rgb { .. })
    ));
    assert!(host.invalidate_count() > 0);
}

/// 回归（review finding 1）：明暗方案变化后的颜色查询在下一次驱动**同步发起**（而不是留在
/// `query_requested` 里永远不发出）；驱动一次就把新颜色应用并重新解析主题。
#[test]
fn scheme_changes_start_their_color_query_synchronously_on_the_next_drive() {
    let _lock = lock();
    reset_theme_state();
    let runtime = runtime();
    let mut host = FakeHost::new();
    host.set_result(light_colors());
    let (mut controller, _signals) =
        create_controller(&mut host, constant_setting(None), Some("light/dark"));
    drop(controller.apply_from_settings(&mut host));
    runtime.block_on(controller.wait_for_terminal_colors(&mut host));
    assert_eq!(theme_name().as_deref(), Some("light"));
    assert_eq!(host.query_count(), 1);

    // 终端报告浅色，但之后颜色查询会说它渲染深色。
    host.set_result(dark_colors());
    host.emit_terminal_color_scheme(TerminalColorScheme::Light);
    // 监听器无法借用 host：查询在下一次驱动时**同步**发起（future 还没被 await）。
    controller.pump(&mut host);
    assert_eq!(host.query_count(), 2, "the queued query starts immediately");
    runtime.block_on(controller.wait_for_terminal_colors(&mut host));
    assert_eq!(theme_name().as_deref(), Some("dark"));
}

/// 回归（review finding 1）：late reply 的颜色更新不再等下一次驱动才请求渲染 —— 共享句柄
/// 立即置位宿主的调度标志（TS 在 `applyTerminalColors()` 里同步 `ui.requestRender()`），
/// `invalidate()` 仍在下一次驱动补上。
#[test]
fn callback_color_updates_request_render_immediately() {
    let _lock = lock();
    reset_theme_state();
    let runtime = runtime();
    let mut host = FakeHost::new();
    host.set_result(TerminalColors::default());
    let (mut controller, _signals) = create_controller(&mut host, constant_setting(None), None);
    runtime.block_on(async {
        drop(controller.apply_from_settings(&mut host));
    });
    let _ = host.take_render_request();

    host.emit_late_reply(dark_colors());
    assert!(
        host.render_requested(),
        "the callback path requests a render immediately"
    );
    assert!(matches!(
        THEME.color(ThemeToken::Error).expect("theme initialized"),
        Some(Color::Rgb { .. })
    ));
}

/// 覆盖（ARCHITECTURE §4.8 item 9）：`rebindTui()` 退订旧监听器、重新绑定并同步通知开关。
#[test]
fn rebind_tui_rebinds_the_scheme_listener_and_notifications() {
    let _lock = lock();
    reset_theme_state();
    let runtime = runtime();
    let mut host = FakeHost::new();
    host.set_result(light_colors());
    let (mut controller, _signals) =
        create_controller(&mut host, constant_setting(None), Some("light/dark"));
    drop(controller.apply_from_settings(&mut host));
    assert_eq!(host.notifications(), vec![true]);

    controller.rebind_tui(&mut host);
    assert_eq!(host.unsubscribe_count(), 1);
    // 新监听器仍然接收上报（autoSync 仍是 true）。
    host.emit_terminal_color_scheme(TerminalColorScheme::Light);
    controller.pump(&mut host);
    assert_eq!(host.query_count(), 2);
    runtime.block_on(controller.wait_for_terminal_colors(&mut host));
}

/// 覆盖（ARCHITECTURE §4.8 item 9）：`setThemeSetting()` 存值后走 `applyFromSettings()`。
#[test]
fn set_theme_setting_stores_the_setting_and_applies_it() {
    let _lock = lock();
    reset_theme_state();
    let runtime = runtime();
    let mut host = FakeHost::new();
    let (mut controller, _signals) = create_controller(&mut host, constant_setting(None), None);

    runtime.block_on(controller.set_theme_setting(&mut host, "light/dark"));
    assert_eq!(
        controller.get_theme_selection().as_deref(),
        Some("light/dark")
    );
    assert_eq!(host.notifications(), vec![true]);
    assert_eq!(theme_name().as_deref(), Some("dark"));

    runtime.block_on(controller.set_theme_setting(&mut host, "light"));
    assert_eq!(controller.get_theme_selection().as_deref(), Some("light"));
    assert_eq!(host.notifications(), vec![true, false]);
    assert_eq!(theme_name().as_deref(), Some("light"));
}
