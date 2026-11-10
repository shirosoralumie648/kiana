//! `cli/startup-ui.ts`（253 行）：启动期 TUI、首次设置。
//!
//! 这些对话框在会话创建之前运行，因此不复用交互模式：每次都用
//! [`create_startup_tui`] 新建一个主屏 TUI，结束后 `clear` + `stop`。
//!
//! ## 宿主驱动
//!
//! TS 的 TUI 由 Node 事件循环驱动；Rust 的 [`TuiMainScreen`] 需要宿主每轮调用
//! `pump_pending_input` / 渲染。本模块的 `show_*` 在等待用户选择时用一个 10 ms 的轮询循环
//! 驱动它（`drive_until_settled`），并通过 `startup_process_terminal` 留下的共享句柄
//! 轮询 `ProcessTerminal` 的 stdin（等价 TS 的事件循环：读入 → 输入处理器 → TUI 队列）。
//!
//! TUI 类型基于 `Rc`，因此这些函数只能运行在 `LocalSet` 上（TS 里是同一个事件循环）。
//!
//! TS 源：`packages/coding-agent/src/cli/startup-ui.ts`、`spec/ca-cli.md` §2.19。
//!
//! ## 本模块包含两类东西
//!
//! 1. **纯判定与数据**：首次设置判定 [`should_run_first_time_setup`] 系列，以及发行版指纹
//!    [`DistributionMetadata`] / [`is_official_distribution`]。它们不碰终端。`app.rs` 的启动
//!    流程调用 [`should_run_first_time_setup_default`]；集成测试调用
//!    [`should_run_first_time_setup`] / [`should_run_first_time_setup_with`]；
//!    [`DistributionMetadata`] / [`is_official_distribution`] 目前只在
//!    [`should_run_first_time_setup_with`] 与本模块的单元测试中使用。
//! 2. **启动期对话框**：[`show_startup_selector`]、[`show_first_time_setup`]、
//!    [`show_startup_input`]。三者共享下面这套生命周期，只是注册的组件不同。
//!
//! 本模块不创建会话、不复用交互模式的 TUI，也不实现普通交互模式的渲染循环。
//!
//! ## 一次对话框的完整生命周期
//!
//! 1. [`create_startup_tui`]（内部走 [`try_create_startup_tui`]）：加载并注册主题、
//!    初始化全局主题与键位，然后构造主屏 TUI。失败时 panic `FatalStartupError`。
//! 2. 调用方 `add_child` + `set_focus`，再 [`start_startup_tui`]（或自行 `start` +
//!    `query_startup_terminal_colors`）启动终端并异步查询终端配色。
//! 3. `drive_until_settled` / `drive_until_settled_with_render`：每 10 ms 泵一次 stdin
//!    与渲染决策，直到组件回调把结果送进 oneshot 通道，或者 stdin EOF。
//! 4. 正常结束：`clear_startup_tui` 同步渲染一帧清空内容并等 25 ms，再
//!    `stop(TuiStopOptions::default())` 恢复终端。
//! 5. stdin EOF：调用方直接返回（`None` / 单元值），**不执行第 4 步**——TS 里进程由
//!    `process.on("exit")` 的终端恢复序列收尾后退出，不会走 `await` 之后的代码。
//!
//! ## 失败与边界
//!
//! - 主题解析/注册失败会让整个启动 TUI 创建失败（`try_create_startup_tui` 返回 `Err`），
//!   不会带着空主题表继续；单个主题文件读取失败则由 [`load_themes`] 静默跳过。
//! - 提交/取消回调只认第一次事件：`Rc<RefCell<Option<Sender>>>` 被 `take()` 之后，重复的
//!   提交/取消是 no-op。这是 TS 里 promise 只能 settle 一次的等价物（主题预览回调除外，
//!   它必须可以被多次调用）。
//! - EOF 判定依赖 `startup_process_terminal` 留下的线程局部弱引用；没有安装终端时所有
//!   泵函数都退化为 no-op / 非 EOF。
//!
//! ## 并发与线程约束
//!
//! 状态全部是 `Rc` / `RefCell` / 线程局部，类型不实现 `Send`。调用方必须在当前线程的
//! `LocalSet` 上 `await` 这些函数（`app.rs` 与 `cli/session_picker.rs` 都是如此）。
//! 组件与宿主本来就在同一线程（`config_selector` 的回调是 `Rc<dyn Fn()>`，
//! `session_picker` 的 `RequestRenderFn` 则是 `Arc<dyn Fn() + Send + Sync>`）；重绘信号
//! 仍走 `tokio::sync::mpsc` 的无界通道，是为了让这两类同步回调都能把“需要重绘”送回
//! async 驱动循环用 `recv().await` 消费，而不是在组件回调里直接碰 `TuiMainScreen`
//! 的借用。

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use kiana_config::settings::{SettingsManager, SettingsManagerCreateOptions};
use kiana_core::config::{APP_NAME, CONFIG_DIR_NAME, ENV_AGENT_DIR, PACKAGE_NAME, get_agent_dir};
use kiana_interactive::components::extension_input::{
    ExtensionInputComponent, ExtensionInputOptions,
};
use kiana_interactive::components::extension_selector::{
    ExtensionSelectorComponent, ExtensionSelectorOptions,
};
use kiana_interactive::components::first_time_setup::{
    FirstTimeSetupComponent, FirstTimeSetupOptions, FirstTimeSetupResult,
};
use kiana_resources::package::{
    DefaultPackageManager, MissingSourceAction, PackageManager, PackageManagerOptions,
    ResolvedResource, missing_source_callback,
};
use kiana_theme::theme_controller::{ThemeHost, request_terminal_colors};
use kiana_theme::{
    SYSTEM_THEME_NAME, get_terminal_theme, init_theme, load_theme_from_path,
    mark_terminal_colors_pending, resolve_theme_setting, set_registered_themes,
    set_terminal_colors, set_theme,
};
use kiana_tui::components::loader::RequestRenderCallback;
use kiana_tui::main_screen::TuiMainScreen;
use kiana_tui::terminal::ProcessTerminal;
use kiana_tui::terminal_colors::{
    Terminal as TerminalTrait, TerminalColorScheme, TerminalColors as TuiTerminalColors,
};
use kiana_tui::tui::{FocusTarget, RenderDecision, Tui, TuiStopOptions};
use std::time::Instant;

use crate::cli::setup::FatalStartupError;

thread_local! {
    /// 最近一次启动期 TUI 使用的 `ProcessTerminal`（弱引用；TUI 销毁后自动失效）。
    ///
    /// `Terminal` trait 的 `poll_input` / `poll_resize` 只有 no-op 默认实现，而
    /// `StartupTerminal` 没有转发它们（`ProcessTerminal::poll` 与 `stdin_eof` 是
    /// 固有方法，同样不在 trait 上）；`create_startup_tui` 的冻结签名只返回
    /// `TuiMainScreen`，因此宿主必须保留这个线程局部共享句柄、直接调用
    /// `ProcessTerminal`，才能像 TS 的事件循环一样把 stdin 送进终端。
    /// 所有启动期 TUI 都在同一个 UI 线程上串行创建，因此一个槽位足够。
    static STARTUP_TERMINAL: RefCell<Option<std::rc::Weak<RefCell<ProcessTerminal>>>> =
        const { RefCell::new(None) };

    /// 迟到颜色回复在 `pump_pending_input` 内**同步**到达、而驱动循环正持有
    /// `RefCell<TuiMainScreen>` 借用时置位。
    ///
    /// TS 的 `queryStartupTerminalColors` 回调在事件循环里直接 `ui.invalidate()` +
    /// `ui.requestRender()`；Rust 侧同一个调用栈上无法再次 `borrow_mut`（kiana-tui 在
    /// `handle_terminal_input` 里同步调用 `onLateReply`），因此回调只置位标记，驱动循环在
    /// `pump_pending_input` 返回、仍持有同一份借用时补做 invalidate + requestRender。
    /// 启动期 TUI 在同一线程串行创建/驱动，一个线程局部标记足够。
    static PENDING_COLOR_APPLY: Cell<bool> = const { Cell::new(false) };
}

/// `ProcessTerminal` 的 `Terminal` 包装：保留共享句柄供宿主轮询 stdin。
///
/// 为什么需要包装：`TuiMainScreen::with_options` 要求 `Box<dyn Terminal>`（所有权转移），
/// 而宿主循环还必须直接操作 `ProcessTerminal`——`ProcessTerminal::poll` 与 `stdin_eof`
/// 是固有方法（不在 `Terminal` trait 上），`poll_input` / `poll_resize` 在 trait 里
/// 只有 no-op 默认实现且包装层并未转发它们。包装层把 `Rc<RefCell<ProcessTerminal>>`
/// 的弱引用留在 `STARTUP_TERMINAL`，自己只做转发，于是 TUI 与宿主共享同一个终端对象。
///
/// 除 `drain_input` 外每个方法都是单行转发；`drain_input` 用共享句柄单独实现了一个
/// 轮询循环（见该方法）：直接转发会让返回的 future 借用 `RefMut` 临时值。
struct StartupTerminal {
    /// 与 TUI 渲染层共享的终端实例（`startup_process_terminal` 创建）。
    inner: Rc<RefCell<ProcessTerminal>>,
}

impl StartupTerminal {
    fn borrow_mut(&self) -> std::cell::RefMut<'_, ProcessTerminal> {
        self.inner.borrow_mut()
    }
}

impl TerminalTrait for StartupTerminal {
    fn start(&mut self, on_input: Box<dyn FnMut(&str)>, on_resize: Box<dyn FnMut()>) {
        self.borrow_mut().start(on_input, on_resize);
    }

    fn stop(&mut self) {
        self.borrow_mut().stop();
    }

    fn restore_on_exit(&mut self, before: &str, after: &str) {
        self.borrow_mut().restore_on_exit(before, after);
    }

    fn drain_input(&mut self, max_ms: u64, idle_ms: u64) -> Pin<Box<dyn Future<Output = ()> + '_>> {
        let inner = Rc::clone(&self.inner);
        Box::pin(async move {
            // 启动期 TUI 不使用 `drainInput`（TS 只在交互模式退出时调用）；这里用共享句柄
            // 做等价的“排空 stdin 直到超时/空闲”。
            //
            // 终止条件是 `max_ms` 与 `idle_ms` 的或：`started` 限制总时长，`last_data`
            // 限制“距最后一次收到数据”的空闲窗口。两者都以 10 ms 为粒度轮询，因此实际
            // 返回值可能比参数多出不到一个睡眠周期。没有任何输入时，`last_data` 与
            // `started` 同时开始计时，函数在 `min(max_ms, idle_ms)` 之后返回——
            // `max_ms` 检查排在 `idle_ms` 之前（10 ms 轮询粒度向上取整）。
            let started = Instant::now();
            let mut last_data = Instant::now();
            loop {
                // 先驱动一次终端：`poll` 处理分片/超时等缓冲事件，`poll_input` 排空 stdin
                // 并把解码结果交给 `Terminal::start` 注册的回调（TUI 的待处理输入队列）。
                let had_data = {
                    let mut terminal = inner.borrow_mut();
                    terminal.poll(Instant::now());
                    terminal.poll_input()
                };
                if had_data {
                    last_data = Instant::now();
                }
                if started.elapsed().as_millis() >= max_ms as u128 {
                    break;
                }
                if last_data.elapsed().as_millis() >= idle_ms as u128 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
    }

    fn write(&mut self, data: &str) {
        self.borrow_mut().write(data);
    }

    fn columns(&self) -> usize {
        self.inner.borrow().columns()
    }

    fn rows(&self) -> usize {
        self.inner.borrow().rows()
    }

    fn kitty_protocol_active(&self) -> bool {
        self.inner.borrow().kitty_protocol_active()
    }

    fn move_by(&mut self, lines: isize) {
        self.borrow_mut().move_by(lines);
    }

    fn hide_cursor(&mut self) {
        self.borrow_mut().hide_cursor();
    }

    fn show_cursor(&mut self) {
        self.borrow_mut().show_cursor();
    }

    fn clear_line(&mut self) {
        self.borrow_mut().clear_line();
    }

    fn clear_from_cursor(&mut self) {
        self.borrow_mut().clear_from_cursor();
    }

    fn clear_screen(&mut self) {
        self.borrow_mut().clear_screen();
    }

    fn set_title(&mut self, title: &str) {
        self.borrow_mut().set_title(title);
    }

    fn set_progress(&mut self, active: bool) {
        self.borrow_mut().set_progress(active);
    }

    fn set_program_status(&mut self, status: &kiana_tui::terminal_colors::ProgramStatus) {
        self.borrow_mut().set_program_status(status);
    }
}

/// 构造启动期 TUI 的终端（注册可轮询的共享句柄）。
///
/// 每次调用都会**覆盖**线程局部槽里的旧弱引用：槽位表示“最近一个启动期终端”，而不是
/// 一个集合。终端返回后由 `TuiMainScreen` 持有，槽里只有 `Weak`，因此 TUI 被丢弃后
/// [`poll_startup_terminal`] / [`startup_stdin_eof`] 自动退化为 no-op，不会延长终端寿命。
///
/// 除本模块的对话框外，`cli/config_selector.rs` 也用它构造 TUI（自己不调用本模块的
/// `create_startup_tui`，但同样需要在驱动循环里泵 stdin）。
pub(crate) fn startup_process_terminal() -> Box<dyn TerminalTrait> {
    let terminal = Rc::new(RefCell::new(ProcessTerminal::new()));
    STARTUP_TERMINAL.with(|slot| {
        *slot.borrow_mut() = Some(Rc::downgrade(&terminal));
    });
    Box::new(StartupTerminal { inner: terminal })
}

/// 宿主循环的一步：把终端读取线程收到的字节喂给终端（`pollInput` + 定时器 + resize）。
///
/// 返回 `true` 表示 stdin 已到达 EOF（TS 的 `process.stdin` `end` 事件：Node 的事件循环
/// 随后耗尽，进程静默退出）。
///
/// 槽里没有终端（未调用 `startup_process_terminal`，或 TUI 已销毁）时直接返回 `false`。
/// 三个泵按顺序执行：`poll`（缓冲事件/超时）、`poll_input`（stdin → TUI 输入队列）、
/// `poll_resize`（尺寸变化 → 触发 resize 回调，回调本身会置位重绘标志）。
pub(crate) fn poll_startup_terminal() -> bool {
    let terminal =
        STARTUP_TERMINAL.with(|slot| slot.borrow().as_ref().and_then(std::rc::Weak::upgrade));
    if let Some(terminal) = terminal {
        let mut terminal = terminal.borrow_mut();
        terminal.poll(Instant::now());
        terminal.poll_input();
        terminal.poll_resize();
        return terminal.stdin_eof();
    }
    false
}

/// stdin 是否已到达 EOF（不泵输入/渲染；见 [`poll_startup_terminal`]）。
pub(crate) fn startup_stdin_eof() -> bool {
    STARTUP_TERMINAL.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(std::rc::Weak::upgrade)
            .is_some_and(|terminal| terminal.borrow().stdin_eof())
    })
}

/// 官方发行版的 npm 包名；[`is_official_distribution`] 的三项精确比较之一。
///
/// 取值与 `kiana_core::config::PACKAGE_NAME` 相同，但这是**判定基准**：发行版元数据与
/// 三个 `OFFICIAL_*` 常量中任一项不一致，就会在 [`should_run_first_time_setup_with`]
/// 里被判为非官方发行版（测试 `tests/first_time_setup_fork.rs` 用只修改 `package_name`
/// 的 fork 覆盖该分支）。
/// 不要把它当成“当前构建的包名”使用。
pub const OFFICIAL_PACKAGE_NAME: &str = "@earendil-works/kiana-coding-agent";
/// 官方发行版期望的应用名（`APP_NAME` 的正常取值，也是 CLI 名）；精确比较、区分大小写。
pub const OFFICIAL_APP_NAME: &str = "kiana";
/// 官方发行版的配置目录名（`~/.kiana` 的目录名部分）；精确比较。
pub const OFFICIAL_CONFIG_DIR_NAME: &str = ".kiana";

/// 发行版元数据（TS 的 `DistributionMetadata`）。
///
/// 三个字段必须**全部**与 `OFFICIAL_*` 常量逐字相等才算官方发行版；字段是 `String`
/// 而不是常量引用，测试因此可以通过显式输入构造任意发行版元数据（见
/// `tests/first_time_setup_fork.rs`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistributionMetadata {
    /// 构建所用的包名（通常来自 `PACKAGE_NAME`）。
    pub package_name: String,
    /// 应用名（通常来自 `APP_NAME`）。
    pub app_name: String,
    /// 配置目录名（通常来自 `CONFIG_DIR_NAME`）。
    pub config_dir_name: String,
}

/// `isOfficialDistribution({packageName, appName, configDirName})`。
///
/// 严格相等比较，不做大小写折叠或路径规范化：三个字段都必须与对应常量逐字相等，任何一项
/// 不同都返回 `false`。首次设置只对官方发行版启用，见
/// [`should_run_first_time_setup_with`]。
pub fn is_official_distribution(metadata: &DistributionMetadata) -> bool {
    metadata.package_name == OFFICIAL_PACKAGE_NAME
        && metadata.app_name == OFFICIAL_APP_NAME
        && metadata.config_dir_name == OFFICIAL_CONFIG_DIR_NAME
}

/// `loadThemes(resources)`：只加载 `enabled` 的主题，按名字去重（先到先得），错误忽略。
///
/// 输入是包管理器解析出的主题资源列表，输出顺序与输入一致。
///
/// - `enabled == false` 的资源直接跳过（配置里“禁用”不等于“删除”）。
/// - 读取/解析失败（文件不存在、JSON 非法、颜色非法）只丢弃该项：启动横幅不应因为一个
///   坏主题而失败，正常的资源加载流程稍后会报告诊断。
/// - 去重按 `Theme::name()` 的字符串：**第一个**同名的主题保留，后续跳过；没有名字的主题
///   不参与去重（全部保留）。
/// - 只返回 `Arc<Theme>`，不注册也不修改全局主题——注册由 `try_create_startup_tui` 里的
///   `set_registered_themes` 负责；`mode` 参数传 `None`，由终端能力决定色彩模式。
pub fn load_themes(resources: &[ResolvedResource]) -> Vec<Arc<kiana_theme::Theme>> {
    let mut themes = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for resource in resources {
        if !resource.enabled {
            continue;
        }
        let Ok(loaded_theme) = load_theme_from_path(Path::new(&resource.path), None) else {
            // 启动提示不应因坏主题失败；正常资源加载器稍后会上报主题诊断。
            continue;
        };
        if let Some(name) = loaded_theme.name()
            && !seen.insert(name.to_string())
        {
            continue;
        }
        themes.push(loaded_theme);
    }
    themes
}

/// `process.cwd()`。
///
/// 取不到当前目录（例如工作目录已被删除）时回退到 `"."`。这里的结果只用作包管理器的
/// 基准路径：回退后解析结果可能偏离真实工作目录，但不会让启动流程崩溃。
fn process_cwd() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string())
}

/// `loadStartupThemes(settingsManager)`：只用全局设置、跳过安装。
///
/// `resolve()` 的失败会向上传播：启动期主题解析失败不应被静默吞掉。
///
/// 实现要点（为什么不是直接用传入的 `settings_manager`）：
/// - 新建一个**内存** `SettingsManager`，只灌入 `get_global_settings()` 的快照；启动横幅
///   出现在项目信任判定之前，项目级设置此时不应参与。`project_trusted: Some(false)`
///   与实际传入的快照一致（没有项目设置可加载）。
/// - `DefaultPackageManager` 用当前进程目录 + `get_agent_dir()`；`builtin_extensions` 为空
///   （本函数只取主题列表，不注册内置扩展）。
/// - `resolve()` 的 `onMissing` 恒返回 `Skip`：启动期绝不安装缺失的源，只解析已有资源。
/// - 失败被转换成 `error.message()` 字符串（返回类型是 `Result<_, String>`），
///   由 [`try_create_startup_tui`] 向上返回；[`create_startup_tui`] 再把它转成
///   [`FatalStartupError`] panic。
pub async fn load_startup_themes(
    settings_manager: &SettingsManager,
) -> Result<Vec<Arc<kiana_theme::Theme>>, String> {
    let global_settings_manager = SettingsManager::in_memory_with_options(
        settings_manager.get_global_settings(),
        SettingsManagerCreateOptions {
            project_trusted: Some(false),
        },
    );
    let package_manager = DefaultPackageManager::new(PackageManagerOptions {
        cwd: process_cwd(),
        agent_dir: get_agent_dir().to_string_lossy().into_owned(),
        settings_manager: global_settings_manager,
        builtin_extensions: Vec::new(),
    });
    let resolved_paths = package_manager
        .resolve(Some(missing_source_callback(
            |_source: String| async move { MissingSourceAction::Skip },
        )))
        .await
        .map_err(|error| error.message().to_string())?;
    Ok(load_themes(&resolved_paths.themes))
}

/// `createStartupTui(settingsManager)` 的 `Result` 版本。
///
/// TS 的这个函数会让 `loadStartupThemes` / `setRegisteredThemes` 的异常拒绝整个 promise；
/// Rust 无法在冻结签名里表达 rejection，因此：
/// - [`try_create_startup_tui`] 返回 `Err(message)`（message 即 TS 的 `error.message`）；
/// - [`create_startup_tui`] 保持签名不变，失败时 panic [`FatalStartupError`]（等价于 CLI 顶层
///   未捕获的拒绝：安装钩子的二进制入口打印 `Error: <message>` 并以退出码 1 结束，不会静默地
///   继续用空主题列表）。
pub async fn try_create_startup_tui(
    settings_manager: &SettingsManager,
) -> Result<TuiMainScreen, String> {
    // 终端能力覆盖必须在任何渲染/主题构造之前写入：它决定色彩模式与图片能力，值变化时
    // kiana-tui 会清空能力缓存，让下一次探测使用新覆盖值。
    kiana_tui::terminal_image::set_capability_overrides(
        settings_manager.get_terminal_capability_overrides(),
    );
    let themes = load_startup_themes(settings_manager).await?;
    // 注册表是“清空后重新登记”的：失败时它已被部分更新；这里直接返回错误，不再继续
    // 构建 TUI（`create_startup_tui` 会把错误转成 panic）。
    set_registered_themes(themes).map_err(|error| error.message().to_string())?;
    // 系统主题在终端上报颜色之前保持灰度。
    mark_terminal_colors_pending();
    // 设置里可能是 `light/dark` 自动值：用当前终端明暗解析出具体主题名；解析失败
    // （例如含 `/` 但不是合法的自动设置）回退 system。
    let theme_name = resolve_theme_setting(
        settings_manager.get_theme_setting().as_deref(),
        get_terminal_theme(),
    )
    .unwrap_or_else(|| SYSTEM_THEME_NAME.to_string());
    // 只加载一次主题，不启动主题文件监听（第二个参数为 false）。
    init_theme(Some(&theme_name), false);

    // TS `setKeybindings(KeybindingsManager.create())`：kiana-config 的 manager 只暴露
    // `get_user_bindings()`，kiana-tui 需要自己的 manager（同一份默认表 + 用户绑定）。
    let app_manager = kiana_config::keybindings::KeybindingsManager::create(get_agent_dir());
    // `KEYBINDINGS`（平台相关的 TUI 默认值 + APP）与用户绑定：与 TS 的
    // `new KeybindingsManager(KEYBINDINGS, userBindings)` 一致。
    let tui_manager = kiana_tui::keybindings::KeybindingsManager::new(
        kiana_config::keybindings::keybindings().clone(),
        app_manager.get_user_bindings(),
    );
    kiana_tui::keybindings::set_keybindings(tui_manager);

    // 每次创建对话框都新建一个 `ProcessTerminal` 并覆盖线程局部槽位：启动期 TUI 之间是
    // 串行使用关系，不存在两个终端同时驱动。
    let agent_dir = get_agent_dir().to_string_lossy().into_owned();
    // 三个参数对应 TS 的 `(terminal, showHardwareCursor, logDirectory)`：第三个是
    // kiana-tui 的调试日志目录（沿用 agent 目录）。重绘日志 `kiana-tui-debug.log`
    // 还需要 `KIANA_TUI_DEBUG_REDRAW=1` 才启用；超宽行崩溃时的
    // `kiana-tui-crash.log` 则直接写入该目录（未提供目录时回退 tmpdir）。
    let mut ui = TuiMainScreen::with_options(
        startup_process_terminal(),
        Some(settings_manager.get_show_hardware_cursor()),
        Some(agent_dir),
    );
    // `clearOnShrink` 只写入渲染器配置，不触发重绘。
    ui.set_clear_on_shrink(settings_manager.get_clear_on_shrink());
    Ok(ui)
}

/// `createStartupTui(settingsManager)`。
///
/// 失败时 panic [`FatalStartupError`]（见 [`try_create_startup_tui`]）：TS 的调用方不会捕获
/// 这个拒绝，CLI 顶层因此以未捕获异常结束（`Error: <message>` + 退出码 1）。
pub async fn create_startup_tui(settings_manager: &SettingsManager) -> TuiMainScreen {
    match try_create_startup_tui(settings_manager).await {
        Ok(ui) => ui,
        // 与 `run_migrations` 相同的约定：由 `install_fatal_startup_panic_hook` 映射成 Node
        // 未捕获异常的可观察行为。默认 panic 会是 `thread 'main' panicked …` + 退出码 101。
        Err(message) => std::panic::panic_any(FatalStartupError { message }),
    }
}

/// `ThemeHost` 适配：[`TuiMainScreen`] 的 `TuiBase` 提供所需的全部方法。
struct StartupThemeHost<'a> {
    ui: &'a mut TuiMainScreen,
}

impl ThemeHost for StartupThemeHost<'_> {
    fn query_terminal_colors(
        &mut self,
        timeout_ms: u64,
        on_late_reply: Option<Box<dyn FnMut(TuiTerminalColors)>>,
    ) -> Pin<Box<dyn Future<Output = TuiTerminalColors> + 'static>> {
        self.ui
            .base_mut()
            .query_terminal_colors_with_late_reply(timeout_ms, on_late_reply)
    }

    fn set_scheme_notifications(&mut self, enabled: bool) {
        self.ui
            .base_mut()
            .set_terminal_color_scheme_notifications(enabled);
    }

    fn add_scheme_listener(&mut self, listener: Box<dyn FnMut(TerminalColorScheme)>) -> usize {
        self.ui.base_mut().on_terminal_color_scheme_change(listener)
    }

    fn remove_scheme_listener(&mut self, id: usize) {
        self.ui.base_mut().remove_terminal_color_scheme_listener(id);
    }

    fn invalidate(&mut self) {
        self.ui.base_mut().invalidate_all();
    }

    fn request_render(&mut self) {
        self.ui.base_mut().request_render(false);
    }

    fn render_requested_handle(&self) -> Option<Rc<Cell<bool>>> {
        Some(self.ui.base().render_requested_handle())
    }
}

/// TS 颜色回调的收尾：`ui.invalidate(); ui.requestRender()`。
///
/// **必须是非强制渲染**：`request_render(true)` 会清空渲染缓存，下一帧走 width-changed 分支
/// 并全量重绘（`\x1b[2J\x1b[H\x1b[3J`，连滚动缓冲区一起清掉）。TS 的
/// `queryStartupTerminalColors` 只请求一次差分渲染，对话框之外的屏幕内容必须保留。
fn repaint_startup_tui(ui: &mut TuiMainScreen) {
    ui.base_mut().invalidate_all();
    ui.request_render(false);
}

/// `queryStartupTerminalColors(ui, onColors)`：不等待结果。
///
/// 返回的 future 必须被继续 poll（超时后到达的回复也会触发 `apply`），因此放到
/// `LocalSet` 上（app 在 `LocalSet` 内调用这些函数）。
///
/// 迟到回复由 kiana-tui 在 `handle_terminal_input` 内**同步**调用 `onLateReply`；如果这发生在
/// 驱动循环持有 `RefCell<TuiMainScreen>` 借用期间（[`drive_until_settled_with_render`] 的
/// `pump_pending_input`），再次 `borrow_mut` 会 panic。因此这里用 `try_borrow_mut`：能借用
/// 时立即像 TS 一样 invalidate + requestRender，否则置位 [`PENDING_COLOR_APPLY`] 交给驱动
/// 循环在 `pump_pending_input` 返回后补做。
fn query_startup_terminal_colors(
    ui: &Rc<RefCell<TuiMainScreen>>,
    mut on_colors: Box<dyn FnMut() + 'static>,
) {
    let apply_ui = Rc::clone(ui);
    // `apply` 可能被调用两次：第一次是查询完成/超时（可能是部分结果），第二次是之后才
    // 到达的迟到回复。顺序固定为“记录颜色 → 调用方的主题回调 → 重绘”：回调里的
    // `resolve_theme_setting(..., get_terminal_theme())` 要读到刚写入的颜色快照。
    let apply = Box::new(move |colors: TuiTerminalColors| {
        set_terminal_colors(&colors);
        on_colors();
        // 借用失败只有一种情况：驱动循环正持有 `RefCell<TuiMainScreen>` 并在其中同步
        // 分发了这条迟到回复。此时不能重入，留给驱动循环补做重绘。
        match apply_ui.try_borrow_mut() {
            Ok(mut ui) => repaint_startup_tui(&mut ui),
            Err(_) => PENDING_COLOR_APPLY.with(|pending| pending.set(true)),
        }
    });
    // 这里短暂借用 UI 只是为了发起查询（`ThemeHost` 需要 `&mut TuiMainScreen`）；返回的
    // future 不借用 UI，随后交给 `LocalSet`，调用方继续驱动循环，不等待查询结果。
    let future = {
        let mut ui = ui.borrow_mut();
        let mut host = StartupThemeHost { ui: &mut ui };
        request_terminal_colors(&mut host, apply)
    };
    tokio::task::spawn_local(future);
}

/// `startStartupTui(ui, settingsManager)`：启动 TUI 并查询终端颜色。
///
/// `start()` 之后终端进入可接收输入的状态、TUI 请求首帧渲染；随后发起终端配色查询。
/// 查询完成或超时（以及之后的迟到回复）时，会用**最新的**终端明暗重新解析主题设置并
/// `set_theme`，因为 `system` 主题依赖终端实际颜色。`set_theme` 的错误被忽略：它内部
/// 已经回退到 system，启动对话框不需要把主题加载失败升级成致命错误（与
/// [`try_create_startup_tui`] 里注册主题失败不同）。
pub fn start_startup_tui(ui: &Rc<RefCell<TuiMainScreen>>, settings_manager: &SettingsManager) {
    ui.borrow_mut().start();
    let theme_setting = settings_manager.get_theme_setting();
    query_startup_terminal_colors(
        ui,
        Box::new(move || {
            let theme_name = resolve_theme_setting(theme_setting.as_deref(), get_terminal_theme())
                .unwrap_or_else(|| SYSTEM_THEME_NAME.to_string());
            let _ = set_theme(&theme_name, false);
        }),
    );
}

/// `clearStartupTui(ui)`：`clear` → `requestRender` → 渲染一帧 → 25 ms。
///
/// TS 的 `requestRender()` 由 Node 事件循环在下面的 25 ms 里执行；Rust 没有后台渲染循环
/// （`drive_until_settled` 已经返回），必须在这里同步渲染清空帧，否则 `ui.stop()` 之前
/// 对话框一行都不会被擦掉，选择器文本会留在屏幕上。
async fn clear_startup_tui(ui: &Rc<RefCell<TuiMainScreen>>) {
    {
        // 先卸载组件再渲染：下一帧与上一帧做差分，逐行写 `\r\x1b[2K` 擦掉对话框
        // （不是全屏清屏，滚动缓冲要保留）。借用必须在 `await` 之前释放。
        let mut ui = ui.borrow_mut();
        ui.base_mut().clear_children();
        ui.request_render(false);
        ui.render_now(false);
    }
    // 保留 TS 的 25 ms 间隔；此时清空帧已经同步渲染完毕，调用方的 `stop()` 只负责恢复
    // 终端模式，不再依赖渲染回调。
    tokio::time::sleep(Duration::from_millis(25)).await;
}

/// 驱动循环的终止原因。
pub(crate) enum DriveOutcome<T> {
    /// 结果送达；`None` 表示通道在送达前关闭（等价 TS 里悬空 promise 的取消路径）。
    Settled(Option<T>),
    /// stdin 到达 EOF。TS 里 `process.stdin` 的 `end` 事件让 Node 事件循环耗尽：进程执行
    /// `process.on("exit")` 的终端恢复后以 0 静默退出，**不会**走出 `await` 之后的代码。
    StdinEof,
}

/// 驱动 TUI 并等待选择结果（TS 的事件循环）。
///
/// 每 10 ms 泵一次输入与渲染；回调只把结果送进通道，`clear`/`stop` 由外层在
/// 通道就绪后执行（此时不再借用 TUI，避免回调内的二次借用）。
pub(crate) async fn drive_until_settled<T>(
    ui: &Rc<RefCell<TuiMainScreen>>,
    receiver: &mut tokio::sync::oneshot::Receiver<T>,
) -> DriveOutcome<T> {
    // 本模块用 `ExtensionSelectorOptions::default()` / `ExtensionInputOptions::default()`
    // 构造组件（不配置倒计时），组件不会持有重绘回调，因此这里只造一个**永远没有消息**的
    // 重绘通道。发送端必须绑定到变量（而不是 `_`）保持通道存活到驱动结束；若通道关闭，
    // `select!` 的 `Some(())` 分支不会匹配，循环只由 10 ms 节拍推进。
    let (_render_tx, mut render_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    drive_until_settled_with_render(ui, receiver, &mut render_rx).await
}

/// [`drive_until_settled`] 的带重绘信号版本。
///
/// 组件的 `requestRender` 回调都是同步的，但类型随调用方而异：`cli/config_selector.rs`
/// 传 `Rc<dyn Fn()>`（`RequestRenderCallback` 定义在 `kiana_tui::components::loader`），
/// `cli/session_picker.rs` 传 `RequestRenderFn = Arc<dyn Fn() + Send + Sync>`
/// （`kiana_interactive::components::session_selector`）。驱动循环是 async 的：
/// `tokio::sync::mpsc` 的无界通道让两类同步回调都能把重绘请求排队（发送不会阻塞），
/// 循环里用 `try_recv` / `recv().await` 消费（这里不用 `Arc`/`Send` 约束，通道两端
/// 都在同一个线程）。
///
/// 调用方必须保证至少一个发送端存活到驱动结束（`cli/session_picker.rs` 与
/// `cli/config_selector.rs` 都保留着原始的 `render_tx` 绑定）；通道关闭后 `select!` 里的
/// `Some(())` 分支不再匹配，重绘只由 TUI 自己的节流决策驱动。
pub(crate) async fn drive_until_settled_with_render<T>(
    ui: &Rc<RefCell<TuiMainScreen>>,
    receiver: &mut tokio::sync::oneshot::Receiver<T>,
    render_rx: &mut tokio::sync::mpsc::UnboundedReceiver<()>,
) -> DriveOutcome<T> {
    // 每一轮的顺序固定：
    // 1. 泵一次终端：把 stdin 的字节送进 TUI 的待处理输入队列；
    // 2. 借用 UI：派发输入、补做被延迟的颜色重绘、汇总重绘信号、读取渲染决策；
    // 3. 在借用之外渲染（`render_now` 内部会再次 `borrow_mut`）；
    // 4. 检查 stdin EOF（在等待之前检查，保证 EOF 不会压在 10 ms 睡眠之后）；
    // 5. 等待结果 / 重绘信号 / 10 ms 节拍三者之一。
    loop {
        poll_startup_terminal();
        let render_now = {
            let mut ui = ui.borrow_mut();
            ui.base_mut().pump_pending_input();
            // 迟到颜色回复可能就在上面的 `pump_pending_input` 里同步到达（借用期间无法
            // 再次 borrow）；现在补做 TS 回调里的 invalidate + requestRender。
            if PENDING_COLOR_APPLY.with(|pending| pending.replace(false)) {
                repaint_startup_tui(&mut ui);
            }
            // 重绘通道里的消息算“立即渲染”；此外 TUI 自己的节流决策
            // （`RenderDecision::Now`，受 16 ms 最小渲染间隔约束）也会触发渲染。
            let mut requested = false;
            while render_rx.try_recv().is_ok() {
                requested = true;
            }
            let decision = ui.base_mut().render_decision();
            requested || matches!(decision, RenderDecision::Now)
        };
        if render_now {
            ui.borrow_mut().render_now(false);
        }
        // TS 在 stdin 关闭后没有待处理 handle：已排队的渲染执行完后事件循环耗尽，执行进程
        // `exit` 处理器（终端恢复序列写 fd 1）后退出。Rust 复刻同一时机：先执行仍挂起的
        // 渲染请求（`requestRender` 的定时器在 stdin `end` 之前到期），再结束驱动循环；
        // 调用方不再执行 `clear`/`stop`，只把退出码交给二进制入口。
        // 先非阻塞地取一次结果：上面 `pump_pending_input` 可能已经派发了按键并让组件
        // 通过 oneshot 结算。若先检查 EOF，同一轮里到达的选择会被 `StdinEof` 覆盖
        // （管道输入 `printf '1\n' | kiana` 的首个按键就会丢失）。
        match receiver.try_recv() {
            Ok(value) => return DriveOutcome::Settled(Some(value)),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                return DriveOutcome::Settled(None);
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
        }
        if startup_stdin_eof() {
            let mut screen = ui.borrow_mut();
            if screen.base().render_requested_handle().get() {
                screen.render_now(false);
            }
            screen.base_mut().run_exit_restore_handler();
            return DriveOutcome::StdinEof;
        }
        tokio::select! {
            result = &mut *receiver => return DriveOutcome::Settled(result.ok()),
            Some(()) = render_rx.recv() => {
                ui.borrow_mut().render_now(false);
            }
            () = tokio::time::sleep(Duration::from_millis(10)) => {}
        }
    }
}

/// [`should_run_first_time_setup`] 的可注入输入。
///
/// `first-time-setup-fork.test.ts` 用 `vi.mock` 改写 `PACKAGE_NAME`；Rust 侧通过这个结构
/// 显式传入，避免依赖编译期常量。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstTimeSetupOptionsInput {
    /// `PACKAGE_NAME`。
    pub package_name: String,
    /// `APP_NAME`。
    pub app_name: String,
    /// `CONFIG_DIR_NAME`。
    pub config_dir_name: String,
    /// `areExperimentalFeaturesEnabled()`（`KIANA_EXPERIMENTAL === "1"`）。
    pub experimental_features: bool,
    /// `process.env[ENV_AGENT_DIR]` 是否存在且非空。
    pub custom_agent_dir: bool,
    /// `settings.json` 路径。
    pub settings_path: PathBuf,
}

impl FirstTimeSetupOptionsInput {
    /// 从当前进程环境与编译期常量构造（TS 的默认参数）。
    ///
    /// - 三个名字来自 `kiana_core::config` 的 `PACKAGE_NAME` / `APP_NAME` /
    ///   `CONFIG_DIR_NAME`；
    /// - `experimental_features` 每次调用都读 `KIANA_EXPERIMENTAL`（只认 `"1"`，见
    ///   `are_experimental_features_enabled`）；
    /// - `custom_agent_dir` 判断 `KIANA_CODING_AGENT_DIR` 是否存在且非空，空串视为未设置
    ///   （与 `get_agent_dir` 的判定一致）。
    pub fn from_environment(settings_path: &Path) -> Self {
        Self {
            package_name: PACKAGE_NAME.to_string(),
            app_name: APP_NAME.to_string(),
            config_dir_name: CONFIG_DIR_NAME.to_string(),
            experimental_features: kiana_config::defaults::are_experimental_features_enabled(),
            custom_agent_dir: std::env::var_os(ENV_AGENT_DIR)
                .is_some_and(|value| !value.is_empty()),
            settings_path: settings_path.to_path_buf(),
        }
    }
}

/// `shouldRunFirstTimeSetup()`（TS 的默认参数 `getSettingsPath()`）。
///
/// 设置路径取 `kiana_core::config::get_settings_path()`（跟随 `KIANA_CODING_AGENT_DIR`）。
pub fn should_run_first_time_setup_default() -> bool {
    should_run_first_time_setup(&kiana_core::config::get_settings_path())
}

/// `shouldRunFirstTimeSetup(settingsPath)`：官方发行版 + 实验特性 + 默认 agent 目录 +
/// `settings.json` 不存在。
///
/// 每次调用重新读取进程环境；启动流程在 `app.rs` 的交互模式分支里调用一次。
pub fn should_run_first_time_setup(settings_path: &Path) -> bool {
    should_run_first_time_setup_with(&FirstTimeSetupOptionsInput::from_environment(settings_path))
}

/// [`should_run_first_time_setup`] 的可注入版本。
///
/// 判定按**短路顺序**执行：① 三个名字全匹配官方发行版 → ② 实验特性开启 →
/// ③ `custom_agent_dir == false` → ④ `settings_path` 不存在。任何一个条件不满足即返回
/// `false`；fork 或关闭实验特性时不会执行文件系统查询。
pub fn should_run_first_time_setup_with(options: &FirstTimeSetupOptionsInput) -> bool {
    if !is_official_distribution(&DistributionMetadata {
        package_name: options.package_name.clone(),
        app_name: options.app_name.clone(),
        config_dir_name: options.config_dir_name.clone(),
    }) {
        return false;
    }
    if !options.experimental_features {
        return false;
    }
    if options.custom_agent_dir {
        return false;
    }
    !options.settings_path.exists()
}

/// `showStartupSelector(settingsManager, title, options)` 的选项项。
///
/// `T` 不需要 `Clone`：选中时 `value` 会被移出选项表交给调用方。
pub struct StartupSelectorOption<T> {
    /// 展示标签（同时是选中时的匹配键）。
    pub label: String,
    /// 对应值。
    pub value: T,
}

/// `showStartupSelector(settingsManager, title, options)`。
///
/// 显示“标题 + 选项列表”的启动期选择器，返回选中项的 `value`：
/// - 确认（`tui.select.confirm`）→ `Some(value)`；
/// - 取消（`tui.select.cancel`）→ `None`；
/// - stdin EOF → 直接返回 `None`，不做 `clear` / `stop`（见 `DriveOutcome::StdinEof`）。
///
/// 匹配用的是**标签字符串相等**（区分大小写），重复标签只会命中第一个。组件回调只生效
/// 一次：Sender 在 `Rc<RefCell<Option<…>>>` 里被 `take()`。
///
/// # Panics
///
/// 与 [`create_startup_tui`] 相同：主题加载或注册失败时 panic `FatalStartupError`。
/// 必须在当前线程的 `LocalSet` 上调用（函数内部依赖 `spawn_local` 与 `Rc`）。
pub async fn show_startup_selector<T>(
    settings_manager: &SettingsManager,
    title: &str,
    options: Vec<StartupSelectorOption<T>>,
) -> Option<T>
where
    T: 'static,
{
    // 这里可能 panic（主题加载/注册失败），后续的清理逻辑只覆盖创建成功的路径。
    let ui = Rc::new(RefCell::new(create_startup_tui(settings_manager).await));
    // 通道装 `Option<T>`：`Some` = 选中，`None` = 取消。Sender 再包一层 `Option` 并在
    // 回调里 `take()`，保证重复的提交/取消只有第一次生效。
    let (sender, mut receiver) = tokio::sync::oneshot::channel::<Option<T>>();
    let settled: Rc<RefCell<Option<tokio::sync::oneshot::Sender<Option<T>>>>> =
        Rc::new(RefCell::new(Some(sender)));
    // `value` 可能不实现 `Clone`，所以留在共享的 Vec 里，选中时按标签定位后 `remove`。
    let options: Rc<RefCell<Vec<StartupSelectorOption<T>>>> = Rc::new(RefCell::new(options));
    // 组件只接受字符串列表；标签在这里复制一份，之后不再随 options 变化。
    let labels: Vec<String> = options.borrow().iter().map(|o| o.label.clone()).collect();

    let select_settled = Rc::clone(&settled);
    let select_options = Rc::clone(&options);
    let on_select = move |label: &str| {
        let mut guard = select_settled.borrow_mut();
        // 已经 settle 过就忽略（等价 promise 只能 resolve 一次）。
        let Some(sender) = guard.take() else {
            return;
        };
        // 标签找不到时 `value` 为 `None`，与取消走同一条路径；正常调用方传入的标签都来自
        // 同一个列表，不会发生。
        let value = {
            let mut options = select_options.borrow_mut();
            options
                .iter()
                .position(|option| option.label == label)
                .map(|index| options.remove(index).value)
        };
        let _ = sender.send(value);
    };
    let cancel_settled = Rc::clone(&settled);
    let on_cancel = move || {
        if let Some(sender) = cancel_settled.borrow_mut().take() {
            let _ = sender.send(None);
        }
    };

    let selector = ExtensionSelectorComponent::new(
        title,
        labels,
        on_select,
        on_cancel,
        ExtensionSelectorOptions::default(),
    );
    {
        // 挂载树里只有选择器一个子项，焦点固定指向索引 0。
        let mut ui = ui.borrow_mut();
        ui.add_child(Box::new(selector));
        ui.set_focus(Some(FocusTarget::Component { index: 0 }));
    }
    start_startup_tui(&ui, settings_manager);

    let result = match drive_until_settled(&ui, &mut receiver).await {
        // 通道里的 `Option<T>` 与通道关闭的 `None` 在这里合并成一个 `Option<T>`。
        DriveOutcome::Settled(value) => value.flatten(),
        // stdin 已关闭：TS 的进程直接退出，不执行 `clear`/`stop`。
        DriveOutcome::StdinEof => return None,
    };
    // 正常结束：先同步渲染清空帧，再恢复终端。
    clear_startup_tui(&ui).await;
    ui.borrow_mut().stop(TuiStopOptions::default());
    result
}

/// `showFirstTimeSetup(settingsManager)`：显示首次设置对话框并持久化结果。
///
/// 对话框分两步（主题 → 匿名用量）。移动主题高亮时 `on_theme_preview` 立即 `set_theme`，
/// 并通过 TUI 的共享重绘句柄请求一帧，用户看到的是真实预览。
///
/// 只有提交才持久化：`set_theme` + `set_enable_analytics` 更新内存并入队写盘，
/// `flush().await` 等待队列排空（`set_enable_analytics(true)` 在缺少 tracking id 时会生成
/// 一个，见 `kiana-config` 的实现与 `tests/first_time_setup.rs`）。取消或 stdin EOF 不写
/// 任何设置。主题加载失败会 panic（同 [`create_startup_tui`]），必须在 `LocalSet` 上调用。
pub async fn show_first_time_setup(settings_manager: &SettingsManager) {
    let ui = Rc::new(RefCell::new(create_startup_tui(settings_manager).await));
    // 与选择器相同的一次性通道；这里载荷是 `Option<FirstTimeSetupResult>`。
    let (sender, mut receiver) = tokio::sync::oneshot::channel::<Option<FirstTimeSetupResult>>();
    let settled: Rc<RefCell<Option<tokio::sync::oneshot::Sender<Option<FirstTimeSetupResult>>>>> =
        Rc::new(RefCell::new(Some(sender)));

    // 直接 `start()` 而不是 `start_startup_tui`：颜色查询的回调要重放“当前预览主题”，
    // 不是设置里的初始主题（见下面的 `query_startup_terminal_colors`）。
    ui.borrow_mut().start();
    // 记录用户高亮到的主题名；颜色迟到时用它重放预览（期间用户可能已改过选择）。
    let preview_theme: Rc<RefCell<String>> = Rc::new(RefCell::new(SYSTEM_THEME_NAME.to_string()));
    // 对话框先以 system 主题开场；system 在终端颜色到达前是灰度。
    let _ = set_theme(SYSTEM_THEME_NAME, false);
    // 预览回调在组件内部同步执行，此时驱动循环可能正持有 `RefCell<TuiMainScreen>`，
    // 不能调用 `request_render`；共享句柄只写标志，由驱动循环统一消费。
    let render_requested: RequestRenderCallback = {
        let handle = ui.borrow().base().render_requested_handle();
        Rc::new(move || handle.set(true))
    };
    // 预览路径：记住名字 → 切换全局主题 → 置位重绘。`set_theme` 失败被忽略，它内部会
    // 回退到 system，启动对话框不应因此中断。
    let preview = Rc::clone(&preview_theme);
    let on_theme_preview = {
        let render_requested = Rc::clone(&render_requested);
        move |theme_name: &str| {
            *preview.borrow_mut() = theme_name.to_string();
            let _ = set_theme(theme_name, false);
            render_requested();
        }
    };
    let submit_settled = Rc::clone(&settled);
    let on_submit = move |result: FirstTimeSetupResult| {
        if let Some(sender) = submit_settled.borrow_mut().take() {
            let _ = sender.send(Some(result));
        }
    };
    let cancel_settled = Rc::clone(&settled);
    let on_cancel = move || {
        if let Some(sender) = cancel_settled.borrow_mut().take() {
            let _ = sender.send(None);
        }
    };
    let component = FirstTimeSetupComponent::new(FirstTimeSetupOptions {
        on_theme_preview: Box::new(on_theme_preview),
        on_submit: Box::new(on_submit),
        on_cancel: Box::new(on_cancel),
    });
    {
        let mut ui = ui.borrow_mut();
        ui.add_child(Box::new(component));
        ui.set_focus(Some(FocusTarget::Component { index: 0 }));
        ui.request_render(false);
    }
    // 颜色到达（或超时、以及迟到回复）时重放**当前预览主题**：`set_terminal_colors` 已经
    // 先写入新快照，system 主题会按新颜色重新生成。
    let preview = Rc::clone(&preview_theme);
    query_startup_terminal_colors(
        &ui,
        Box::new(move || {
            let _ = set_theme(&preview.borrow(), false);
        }),
    );

    let result = match drive_until_settled(&ui, &mut receiver).await {
        DriveOutcome::Settled(value) => value.flatten(),
        // stdin 已关闭：TS 的进程直接退出，不执行 `clear`/`stop`，也不保存设置。
        DriveOutcome::StdinEof => return,
    };
    if let Some(result) = result {
        // 提交才落盘：两个 setter 都是“更新内存 + 入队写任务”，`flush` 是队列屏障，
        // 返回时此前的写任务已经执行完。
        settings_manager.set_theme(result.theme);
        settings_manager.set_enable_analytics(result.share_analytics);
        settings_manager.flush().await;
    }
    // 取消（`None`）跳过持久化，但仍然要擦除对话框并恢复终端。
    clear_startup_tui(&ui).await;
    ui.borrow_mut().stop(TuiStopOptions::default());
}

/// `showStartupInput(settingsManager, title, placeholder?)`。
///
/// 单行输入对话框：确认时返回输入文本 `Some(text)`，取消返回 `None`，stdin EOF 直接返回
/// `None`（不做 `clear` / `stop`）。空输入也算一次有效提交（组件不校验内容）。
///
/// `placeholder` 只被原样转交给组件；当前 `ExtensionInputComponent::new` 的对应参数名为
/// `_placeholder`，即它不影响显示内容。主题加载失败时 panic（同 [`create_startup_tui`]），
/// 必须在 `LocalSet` 上调用。
pub async fn show_startup_input(
    settings_manager: &SettingsManager,
    title: &str,
    placeholder: Option<&str>,
) -> Option<String> {
    let ui = Rc::new(RefCell::new(create_startup_tui(settings_manager).await));
    // 一次性通道：`Some(text)` = 提交，`None` = 取消。
    let (sender, mut receiver) = tokio::sync::oneshot::channel::<Option<String>>();
    let settled: Rc<RefCell<Option<tokio::sync::oneshot::Sender<Option<String>>>>> =
        Rc::new(RefCell::new(Some(sender)));

    let submit_settled = Rc::clone(&settled);
    let on_submit = move |value: &str| {
        // 输入是借用，复制成 String；Sender 被取走后重复提交是 no-op。
        if let Some(sender) = submit_settled.borrow_mut().take() {
            let _ = sender.send(Some(value.to_string()));
        }
    };
    let cancel_settled = Rc::clone(&settled);
    let on_cancel = move || {
        if let Some(sender) = cancel_settled.borrow_mut().take() {
            let _ = sender.send(None);
        }
    };
    // 没有 timeout 时 `dispose()` 只是释放倒计时；组件随 `clear()` 一起丢弃。
    let input = ExtensionInputComponent::new(
        title,
        placeholder,
        on_submit,
        on_cancel,
        ExtensionInputOptions::default(),
    );
    {
        // 输入框是挂载树里唯一的子项，焦点固定指向索引 0。
        let mut ui = ui.borrow_mut();
        ui.add_child(Box::new(input));
        ui.set_focus(Some(FocusTarget::Component { index: 0 }));
    }
    start_startup_tui(&ui, settings_manager);

    let result = match drive_until_settled(&ui, &mut receiver).await {
        DriveOutcome::Settled(value) => value.flatten(),
        // stdin 已关闭：TS 的进程直接退出，不执行 `clear`/`stop`。
        DriveOutcome::StdinEof => return None,
    };
    // 先擦除对话框再恢复终端（与选择器同一收尾）。
    clear_startup_tui(&ui).await;
    ui.borrow_mut().stop(TuiStopOptions::default());
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiana_tui::tui::Component;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// 捕获 `ProcessTerminal::with_output` 写出的字节。
    #[derive(Clone)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for SharedWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// 固定行数的展示组件（不处理输入，等价 TS 的静态 Component）。
    struct StaticLines(Vec<String>);

    impl Component for StaticLines {
        fn render(&mut self, _width: usize) -> Vec<String> {
            self.0.clone()
        }

        fn handles_input(&self) -> bool {
            false
        }
    }

    fn capture_screen() -> (TuiMainScreen, Arc<Mutex<Vec<u8>>>) {
        let output = Arc::new(Mutex::new(Vec::<u8>::new()));
        let screen = TuiMainScreen::with_options(
            Box::new(ProcessTerminal::with_output(Box::new(SharedWriter(
                Arc::clone(&output),
            )))),
            None,
            None,
        );
        (screen, output)
    }

    fn drain_written(output: &Arc<Mutex<Vec<u8>>>, from: usize) -> String {
        let bytes = output
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        String::from_utf8_lossy(&bytes[from..]).into_owned()
    }

    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var_os(key);
            match value {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    fn official_input(settings_path: &Path) -> FirstTimeSetupOptionsInput {
        FirstTimeSetupOptionsInput {
            package_name: OFFICIAL_PACKAGE_NAME.to_string(),
            app_name: OFFICIAL_APP_NAME.to_string(),
            config_dir_name: OFFICIAL_CONFIG_DIR_NAME.to_string(),
            experimental_features: true,
            custom_agent_dir: false,
            settings_path: settings_path.to_path_buf(),
        }
    }

    #[test]
    fn official_distribution_requires_all_three_names() {
        let metadata = DistributionMetadata {
            package_name: OFFICIAL_PACKAGE_NAME.to_string(),
            app_name: OFFICIAL_APP_NAME.to_string(),
            config_dir_name: OFFICIAL_CONFIG_DIR_NAME.to_string(),
        };
        assert!(is_official_distribution(&metadata));
        let mut forked = metadata.clone();
        forked.package_name = "@example/kiana-coding-agent".to_string();
        assert!(!is_official_distribution(&forked));
        let mut forked = metadata.clone();
        forked.app_name = "fork".to_string();
        assert!(!is_official_distribution(&forked));
        let mut forked = metadata.clone();
        forked.config_dir_name = ".fork".to_string();
        assert!(!is_official_distribution(&forked));
    }

    #[test]
    fn first_time_setup_returns_true_for_official_environment() {
        let temp = tempfile::tempdir().expect("tempdir");
        let settings_path = temp.path().join("settings.json");
        assert!(should_run_first_time_setup_with(&official_input(
            &settings_path
        )));
    }

    #[test]
    fn first_time_setup_returns_false_when_experimental_disabled() {
        let temp = tempfile::tempdir().expect("tempdir");
        let settings_path = temp.path().join("settings.json");
        let mut options = official_input(&settings_path);
        options.experimental_features = false;
        assert!(!should_run_first_time_setup_with(&options));
    }

    #[test]
    fn first_time_setup_returns_false_with_custom_agent_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let settings_path = temp.path().join("settings.json");
        let mut options = official_input(&settings_path);
        options.custom_agent_dir = true;
        assert!(!should_run_first_time_setup_with(&options));
    }

    #[test]
    fn first_time_setup_returns_false_when_settings_exist() {
        let temp = tempfile::tempdir().expect("tempdir");
        let settings_path = temp.path().join("settings.json");
        std::fs::write(&settings_path, "{}").expect("write settings");
        assert!(!should_run_first_time_setup_with(&official_input(
            &settings_path
        )));
    }

    #[test]
    fn fork_distribution_never_runs_first_time_setup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let settings_path = temp.path().join("settings.json");
        let mut options = official_input(&settings_path);
        options.package_name = "@example/kiana-coding-agent".to_string();
        assert!(!should_run_first_time_setup_with(&options));
    }

    #[test]
    fn default_environment_reads_experimental_flag_and_agent_dir() {
        let _lock = env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let settings_path = temp.path().join("settings.json");
        let _experimental = EnvGuard::set("KIANA_EXPERIMENTAL", Some("1"));
        let _agent_dir = EnvGuard::set(ENV_AGENT_DIR, None);
        assert!(should_run_first_time_setup(&settings_path));

        let _agent_dir = EnvGuard::set(ENV_AGENT_DIR, Some(temp.path().to_str().unwrap()));
        assert!(!should_run_first_time_setup(&settings_path));

        let _agent_dir = EnvGuard::set(ENV_AGENT_DIR, None);
        let _experimental = EnvGuard::set("KIANA_EXPERIMENTAL", None);
        assert!(!should_run_first_time_setup(&settings_path));
    }

    /// 手动 TTY 测试：真实打开启动选择器（cargo test 默认跳过）。
    #[test]
    #[ignore = "manual TTY test: opens an interactive selector"]
    fn show_startup_selector_manually() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let local = tokio::task::LocalSet::new();
        let temp = tempfile::tempdir().expect("tempdir");
        local.block_on(&runtime, async move {
            let settings_manager = SettingsManager::create_with_options(
                temp.path().to_str().expect("utf-8 path"),
                temp.path().to_str().expect("utf-8 path"),
                SettingsManagerCreateOptions {
                    project_trusted: Some(false),
                },
            );
            let result = show_startup_selector(
                &settings_manager,
                "Pick one",
                vec![
                    StartupSelectorOption {
                        label: "First".to_string(),
                        value: 1u32,
                    },
                    StartupSelectorOption {
                        label: "Second".to_string(),
                        value: 2u32,
                    },
                ],
            )
            .await;
            assert!(matches!(result, Some(1) | Some(2) | None));
        });
    }

    /// 手动 TTY 测试：真实打开首次设置对话框。
    #[test]
    #[ignore = "manual TTY test: opens the first-time setup dialog"]
    fn show_first_time_setup_manually() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let local = tokio::task::LocalSet::new();
        let temp = tempfile::tempdir().expect("tempdir");
        local.block_on(&runtime, async move {
            let settings_manager = SettingsManager::create_with_options(
                temp.path().to_str().expect("utf-8 path"),
                temp.path().to_str().expect("utf-8 path"),
                SettingsManagerCreateOptions {
                    project_trusted: Some(false),
                },
            );
            show_first_time_setup(&settings_manager).await;
        });
    }

    /// 回归：迟到颜色回复在驱动循环持有 `RefCell` 借用时到达，不得触发
    /// “RefCell already borrowed” panic（TS 的 onLateReply 在事件循环里直接 invalidate +
    /// requestRender）。
    #[test]
    fn late_terminal_color_reply_applies_without_reentering_the_ui_borrow() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let local = tokio::task::LocalSet::new();
        local.block_on(&runtime, async {
            let ui = Rc::new(RefCell::new(TuiMainScreen::with_options(
                Box::new(ProcessTerminal::with_output(Box::new(Vec::<u8>::new()))),
                None,
                None,
            )));
            let calls = Rc::new(Cell::new(0u32));
            let calls_clone = Rc::clone(&calls);
            query_startup_terminal_colors(
                &ui,
                Box::new(move || calls_clone.set(calls_clone.get() + 1)),
            );
            // 等过 100 ms 查询超时：之后的颜色回复走 onLateReply。
            tokio::time::sleep(Duration::from_millis(150)).await;

            // 模拟驱动循环：在持有 TUI 借用时分发迟到回复（修复前这里会 panic）。
            {
                let mut screen = ui.borrow_mut();
                screen
                    .base_mut()
                    .handle_terminal_input("\u{1b}]11;#ffffff\u{7}");
                screen.base_mut().handle_terminal_input("\u{1b}[?62;22c");
                assert!(
                    PENDING_COLOR_APPLY.with(|pending| pending.get()),
                    "late reply must schedule the deferred invalidate"
                );
                if PENDING_COLOR_APPLY.with(|pending| pending.replace(false)) {
                    repaint_startup_tui(&mut screen);
                }
                assert!(screen.base().render_requested_handle().get());
            }
            // 一次来自超时（部分结果），一次来自迟到回复。
            assert_eq!(calls.get(), 2);
        });
    }

    /// TS `queryStartupTerminalColors` 的回调是 `ui.invalidate(); ui.requestRender()`
    /// （非强制）。强制渲染会清空渲染缓存，下一帧走 width-changed 全量重绘（`\x1b[2J` +
    /// `\x1b[3J`），把用户的滚动缓冲一起清掉。
    #[test]
    fn terminal_color_repaint_is_differential_and_keeps_scrollback() {
        let (mut screen, output) = capture_screen();
        screen.add_child(Box::new(StaticLines(vec![
            "dialog line 1".to_string(),
            "dialog line 2".to_string(),
        ])));
        screen.render_now(false);
        let before = output
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len();

        repaint_startup_tui(&mut screen);
        assert!(
            screen.base().render_requested_handle().get(),
            "invalidate + requestRender must schedule a frame"
        );
        screen.render_now(false);

        let written = drain_written(&output, before);
        assert!(
            !written.contains("\u{1b}[2J"),
            "unexpected clear: {written:?}"
        );
        assert!(
            !written.contains("\u{1b}[3J"),
            "unexpected scrollback clear: {written:?}"
        );
    }

    /// TS 的 `clearStartupTui` 在 `ui.stop()` 之前一定会渲染清空帧（Node 事件循环服务
    /// `requestRender`），否则对话框文本会留在屏幕上。
    #[tokio::test]
    async fn clear_startup_tui_renders_the_erased_frame_before_stop() {
        let (mut screen, output) = capture_screen();
        screen.add_child(Box::new(StaticLines(vec![
            "dialog line 1".to_string(),
            "dialog line 2".to_string(),
            "dialog line 3".to_string(),
        ])));
        screen.render_now(false);
        let before = output
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len();

        let ui = Rc::new(RefCell::new(screen));
        clear_startup_tui(&ui).await;

        let written = drain_written(&output, before);
        assert!(
            written.contains("\r\u{1b}[2K"),
            "the cleared frame must erase the dialog lines: {written:?}"
        );
        assert!(
            !written.contains("\u{1b}[2J"),
            "unexpected clear: {written:?}"
        );
        assert!(!written.contains("dialog line 1"), "{written:?}");

        // stop() 只能移动光标；擦除必须已经发生。
        let before_stop = output
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len();
        ui.borrow_mut().stop(TuiStopOptions::default());
        assert!(!drain_written(&output, before_stop).contains("dialog line 1"));
    }

    /// TS `createStartupTui` 的拒绝在 CLI 顶层是未捕获异常：Node 打印 `Error: <message>`
    /// 并以 1 退出。Rust 用 [`FatalStartupError`] panic，由启动钩子映射成同样的可观察行为
    /// （默认 panic 会打印 `thread 'main' panicked …` 并以 101 退出）。
    #[test]
    fn create_startup_tui_raises_a_fatal_startup_error() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let local = tokio::task::LocalSet::new();
        let mut settings = kiana_config::settings::Settings::default();
        settings
            .extra
            .insert("packages".to_string(), serde_json::json!([42]));
        let settings_manager = SettingsManager::in_memory(settings);

        let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            local.block_on(&runtime, async {
                let _ = create_startup_tui(&settings_manager).await;
            });
        }))
        .expect_err("create_startup_tui must not continue with a failed theme setup");
        let fatal = payload
            .downcast_ref::<FatalStartupError>()
            .expect("panic payload must be FatalStartupError");
        assert!(
            fatal.message.contains("startsWith"),
            "unexpected error message: {}",
            fatal.message
        );
    }

    /// 与 [`create_startup_tui_raises_a_fatal_startup_error`] 同一路径的端到端回归：安装了
    /// 启动钩子的二进制打印 `Error: <message>` 并以 1 退出（而不是 Rust 的 panic + 101）。
    #[test]
    fn create_startup_tui_failure_exits_with_status_one() {
        const CHILD_ENV: &str = "KIANA_STARTUP_TUI_FATAL_CHILD";
        if std::env::var_os(CHILD_ENV).is_none() {
            let executable = std::env::current_exe().expect("test executable");
            let output = std::process::Command::new(executable)
                .args([
                    "--exact",
                    "cli::startup_ui::tests::create_startup_tui_failure_exits_with_status_one",
                ])
                .env(CHILD_ENV, "1")
                .output()
                .expect("run child test");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
            assert!(stderr.starts_with("Error: "), "{stderr}");
            assert!(stderr.contains("startsWith"), "{stderr}");
            assert!(!stderr.contains("panicked"), "{stderr}");
            return;
        }

        crate::cli::setup::install_fatal_startup_panic_hook();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let local = tokio::task::LocalSet::new();
        let mut settings = kiana_config::settings::Settings::default();
        settings
            .extra
            .insert("packages".to_string(), serde_json::json!([42]));
        let settings_manager = SettingsManager::in_memory(settings);
        local.block_on(&runtime, async {
            let _ = create_startup_tui(&settings_manager).await;
        });
    }

    /// TS `loadStartupThemes` 没有 try/catch：resolve 失败会拒绝整个 `createStartupTui`。
    #[tokio::test]
    async fn startup_theme_resolution_errors_are_propagated() {
        let mut settings = kiana_config::settings::Settings::default();
        settings
            .extra
            .insert("packages".to_string(), serde_json::json!([42]));
        let settings_manager = SettingsManager::in_memory(settings);
        let error = try_create_startup_tui(&settings_manager)
            .await
            .err()
            .expect("invalid packages setting must fail theme resolution");
        assert!(
            error.contains("startsWith"),
            "unexpected error message: {error}"
        );
    }

    /// TS `setRegisteredThemes` 对含 `/` 的主题名抛错，`createStartupTui` 因此 reject。
    #[tokio::test]
    async fn invalid_registered_theme_names_are_propagated() {
        let temp = tempfile::tempdir().expect("tempdir");
        let theme_path = temp.path().join("bad-theme.json");
        let mut theme: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/package_command/theme.json"
        ))
        .expect("theme fixture");
        theme["name"] = serde_json::json!("bad/name");
        std::fs::write(
            &theme_path,
            serde_json::to_string(&theme).expect("serialize theme"),
        )
        .expect("write theme");
        let settings = kiana_config::settings::Settings {
            themes: Some(vec![theme_path.to_string_lossy().into_owned()]),
            ..Default::default()
        };
        let settings_manager = SettingsManager::in_memory(settings);
        let error = try_create_startup_tui(&settings_manager)
            .await
            .err()
            .expect("invalid theme name must fail theme registration");
        assert!(
            error.contains("Invalid theme name \"bad/name\""),
            "unexpected error message: {error}"
        );
    }

    #[test]
    fn load_themes_skips_disabled_and_duplicates() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("first.json");
        let second = temp.path().join("second.json");
        let theme = include_str!("../../tests/fixtures/package_command/theme.json");
        std::fs::write(&first, theme).expect("write first");
        std::fs::write(&second, theme).expect("write second");
        let resources = vec![
            ResolvedResource {
                path: first.to_string_lossy().into_owned(),
                enabled: true,
                metadata: kiana_core::source_info::PathMetadata {
                    source: "user".to_string(),
                    scope: kiana_core::source_info::SourceScope::User,
                    origin: kiana_core::source_info::SourceOrigin::Package,
                    base_dir: None,
                    package_root: None,
                },
            },
            ResolvedResource {
                path: second.to_string_lossy().into_owned(),
                enabled: true,
                metadata: kiana_core::source_info::PathMetadata {
                    source: "user".to_string(),
                    scope: kiana_core::source_info::SourceScope::User,
                    origin: kiana_core::source_info::SourceOrigin::Package,
                    base_dir: None,
                    package_root: None,
                },
            },
            ResolvedResource {
                path: "/does/not/exist.json".to_string(),
                enabled: true,
                metadata: kiana_core::source_info::PathMetadata {
                    source: "user".to_string(),
                    scope: kiana_core::source_info::SourceScope::User,
                    origin: kiana_core::source_info::SourceOrigin::Package,
                    base_dir: None,
                    package_root: None,
                },
            },
            ResolvedResource {
                path: first.to_string_lossy().into_owned(),
                enabled: false,
                metadata: kiana_core::source_info::PathMetadata {
                    source: "user".to_string(),
                    scope: kiana_core::source_info::SourceScope::User,
                    origin: kiana_core::source_info::SourceOrigin::Package,
                    base_dir: None,
                    package_root: None,
                },
            },
        ];
        let themes = load_themes(&resources);
        assert_eq!(themes.len(), 1);
        assert_eq!(themes[0].name(), Some("dark"));
    }
}
