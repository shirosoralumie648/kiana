//! `cli/config-selector.ts`（61 行）：`kiana config` 选择器。
//!
//! TS 的 `selectConfig` 自己初始化主题（与启动选择器不同：不加载启动主题、不查询终端颜色），
//! 关闭时 `stopThemeWatcher()`；退出键调用 `ui.stop(); stopThemeWatcher(); process.exit(0)`。
//! Rust 侧把退出映射为 `Err(CliExit { code: 0 })`（不终止进程）。
//!
//! TS 源：`packages/coding-agent/src/cli/config-selector.ts`、`spec/ca-cli.md` §2.18。
//!
//! # 执行流程
//! 1. `init_theme(settingsManager.getTheme(), true)` 初始化全局主题，`true` 表示同时启动
//!    主题 watcher；主题非法时 `init_theme` 静默回退到 `system`，不返回错误；
//! 2. 用 `startup_process_terminal()` 创建 TUI 主屏幕，读取 `show_hardware_cursor` /
//!    `clear_on_shrink` 设置与终端行数；
//! 3. 构造 `ConfigSelectorComponent`，把 `on_close` / `on_exit` / `request_render` 三个回调
//!    交给组件；
//! 4. `drive_until_settled_with_render` 泵输入与渲染，并等待结果信号；
//! 5. 结果送达 → `ui.stop()` + `stop_theme_watcher()`，再按 `Close` / `Exit` 返回；
//!    stdin EOF → 只返回 `Err(CliExit(0))`，不执行 stop（见下）。
//!
//! # 为什么结果回调不直接 stop
//! TS 在 `onClose` / `onExit` 回调里直接 `ui.stop()`（`onExit` 随后 `process.exit(0)`）。
//! Rust 侧回调运行在 TUI 的借用期间，若在回调里再次 `borrow_mut()` TUI 会 panic；
//! 因此回调只通过 oneshot 发信号，由驱动循环返回后的外层统一 stop。这也是
//! `drive_until_settled`（`startup_ui.rs`）文档注释里“回调内避免二次借用”约束的由来。
//!
//! # 调用方
//! `package_manager_cli::handle_config_command`（`kiana config`）构造
//! [`ConfigSelectorOptions`] 并等待本函数；返回后由它 flush 设置写队列，再把
//! `Err(CliExit)` 的退出码转成包命令结果。settings 的改写发生在组件内部，本文件不写盘。

use std::cell::RefCell;
use std::rc::Rc;

use kiana_config::settings::SettingsManager;
use kiana_interactive::components::config_selector::{
    ConfigSelectorComponent, ConfigSelectorOptions as InteractiveConfigSelectorOptions,
    ConfigWriteScope, ScopedResolvedPaths,
};
use kiana_theme::{init_theme, stop_theme_watcher};
use kiana_tui::main_screen::TuiMainScreen;
use kiana_tui::tui::{FocusTarget, Tui, TuiStopOptions};

use crate::cli::setup::{CliExit, CliResult};
use crate::cli::startup_ui::{
    DriveOutcome, drive_until_settled_with_render, startup_process_terminal,
};

/// `ConfigSelectorOptions`（TS 的同名接口）。
///
/// `resolved_paths` 以引用传入（组件内部克隆建组），`settings_manager` / `cwd` /
/// `agent_dir` 按值传入组件。`write_scope` 决定初始写入作用域，
/// `project_mode_available` 决定能否用 Tab 切到项目作用域。调用方
/// `package_manager_cli::handle_config_command` 用 `--local` 选择 `Project`，
/// 并用 `settings_manager.is_project_trusted()` 填 `project_mode_available`。
pub struct ConfigSelectorOptions {
    /// 全局/项目两套已解析资源。
    pub resolved_paths: ScopedResolvedPaths,
    /// 设置管理器。
    pub settings_manager: SettingsManager,
    /// 工作目录。
    pub cwd: String,
    /// agent 目录。
    pub agent_dir: String,
    /// 初始写入作用域。
    pub write_scope: ConfigWriteScope,
    /// 是否允许切换到项目模式。
    pub project_mode_available: bool,
}

/// 组件回调送达的结果；只做信号，不在这里执行 `stop`。
enum ConfigOutcome {
    /// `onClose`：正常关闭。
    Close,
    /// `onExit`：请求退出进程（`process.exit(0)`）。
    Exit,
}

/// `selectConfig(options)`：显示 TUI 配置选择器，关闭时返回。
///
/// # 返回
/// - 用户正常关闭（组件 `onClose`）→ `Ok(())`；调用方随后 flush 设置写队列。
/// - 用户按退出键（组件 `onExit`）→ `Err(CliExit::new(0))`，等价 TS 的 `process.exit(0)`。
/// - stdin 在结果送达前 EOF → 同样 `Err(CliExit::new(0))`，但**不**调用 `stop()` 或
///   `stop_theme_watcher()`：复刻 TS 里事件循环耗尽、只有进程 exit 处理器写终端恢复序列的
///   路径（`DriveOutcome::StdinEof` 分支）。
///
/// 本函数没有 `?`，不会产生其它错误。
///
/// # 资源与借用
/// - 终端由 `startup_process_terminal()` 提供，`TuiMainScreen` 接管；正常路径
///   `ui.stop(TuiStopOptions::default())` 后关闭主题 watcher。
/// - `on_close` / `on_exit` 共享一个 `Rc<RefCell<Option<Sender>>>`，发送前 `take()`，
///   因此先到的那个回调生效、`receiver` 只 resolve 一次；两个回调都不触发时驱动循环会
///   继续泵输入直到 stdin EOF。
/// - `request_render` 向无界 mpsc 通道发送重绘请求（发送不会阻塞 UI 回调），由
///   `drive_until_settled_with_render` 消费。
/// - 回调只持有 sender 槽，不借用 `Tui`，避免与驱动循环的 `borrow_mut()` 冲突。
pub async fn select_config(options: ConfigSelectorOptions) -> CliResult<()> {
    // 显示 TUI 之前先初始化主题。
    init_theme(options.settings_manager.get_theme().as_deref(), true);

    let mut ui = TuiMainScreen::with_options(
        startup_process_terminal(),
        Some(options.settings_manager.get_show_hardware_cursor()),
        Some(options.agent_dir.clone()),
    );
    ui.set_clear_on_shrink(options.settings_manager.get_clear_on_shrink());
    // 组件需要终端初始行数作为列表窗口高度；先从 TUI 读一次（之后整个 UI 由 Rc<RefCell> 持有）。
    let terminal_height = ui.terminal().borrow().rows();

    let ui = Rc::new(RefCell::new(ui));
    // 结果信号：组件回调只发送，不直接操作 TUI（避免回调内二次借用）。
    let (sender, mut receiver) = tokio::sync::oneshot::channel::<ConfigOutcome>();
    let settled: Rc<RefCell<Option<tokio::sync::oneshot::Sender<ConfigOutcome>>>> =
        Rc::new(RefCell::new(Some(sender)));
    let (render_tx, mut render_rx) = tokio::sync::mpsc::unbounded_channel::<()>();

    // 两个回调共享同一个 sender 槽：先触发者 take() 后发送，后触发者拿不到 sender，
    // 因此 receiver 只会 resolve 一次。
    let close_settled = Rc::clone(&settled);
    let on_close: Rc<dyn Fn()> = Rc::new(move || {
        let mut guard = close_settled.borrow_mut();
        if let Some(sender) = guard.take() {
            let _ = sender.send(ConfigOutcome::Close);
        }
    });
    let exit_settled = Rc::clone(&settled);
    let on_exit: Rc<dyn Fn()> = Rc::new(move || {
        let mut guard = exit_settled.borrow_mut();
        if let Some(sender) = guard.take() {
            let _ = sender.send(ConfigOutcome::Exit);
        }
    });
    let request_render: Rc<dyn Fn()> = {
        let render_tx = render_tx.clone();
        // 无界通道：组件在同步回调里发送重绘请求不会阻塞；接收端在驱动循环中消费。
        Rc::new(move || {
            let _ = render_tx.send(());
        })
    };

    let selector = ConfigSelectorComponent::new(
        &options.resolved_paths,
        options.settings_manager.clone(),
        options.cwd.clone(),
        options.agent_dir.clone(),
        on_close,
        on_exit,
        request_render,
        InteractiveConfigSelectorOptions {
            terminal_height: Some(terminal_height),
            write_scope: options.write_scope,
            project_mode_available: options.project_mode_available,
        },
    );

    {
        let mut ui = ui.borrow_mut();
        ui.add_child(Box::new(selector));
        // TS: `ui.setFocus(selector.getResourceList())`；组件把焦点转发给资源列表。
        ui.set_focus(Some(FocusTarget::Component { index: 0 }));
        ui.start();
    }

    // 驱动循环只负责泵输入/渲染与等待信号；资源清理统一放在下面返回后执行。
    let outcome = drive_until_settled_with_render(&ui, &mut receiver, &mut render_rx).await;
    match outcome {
        // stdin 关闭：TS 的 promise 永不 resolve，Node 事件循环耗尽后静默退出 0；
        // 不走 `ui.stop()` / `stopThemeWatcher()`（只有进程 exit 处理器写终端恢复序列）。
        DriveOutcome::StdinEof => Err(CliExit::new(0)),
        DriveOutcome::Settled(outcome) => {
            ui.borrow_mut().stop(TuiStopOptions::default());
            stop_theme_watcher();
            match outcome.unwrap_or(ConfigOutcome::Close) {
                ConfigOutcome::Close => Ok(()),
                ConfigOutcome::Exit => Err(CliExit::new(0)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手动 TTY 测试：真实打开 `kiana config` 选择器（Esc 关闭 / Ctrl+C 退出）。
    #[test]
    #[ignore = "manual TTY test: opens the config selector"]
    fn select_config_manually() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let local = tokio::task::LocalSet::new();
        let temp = tempfile::tempdir().expect("tempdir");
        local.block_on(&runtime, async move {
            let settings_manager =
                SettingsManager::in_memory(kiana_config::settings::Settings::default());
            let mut resolved_paths =
                kiana_interactive::components::config_selector::ScopedResolvedPaths::new();
            resolved_paths.insert(
                ConfigWriteScope::Global,
                kiana_resources::package::ResolvedPaths::default(),
            );
            resolved_paths.insert(
                ConfigWriteScope::Project,
                kiana_resources::package::ResolvedPaths::default(),
            );
            let result = select_config(ConfigSelectorOptions {
                resolved_paths,
                settings_manager,
                cwd: temp.path().to_string_lossy().into_owned(),
                agent_dir: temp.path().to_string_lossy().into_owned(),
                write_scope: ConfigWriteScope::Global,
                project_mode_available: false,
            })
            .await;
            assert!(result.is_ok() || result.is_err());
        });
    }
}
