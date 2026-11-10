//! `cli/session-picker.ts`（55 行）：`--resume` 选择器。
//!
//! TS 的 `selectSession` 返回 `string | null`；`onExit`（用户按退出键）调用
//! `ui.stop(); process.exit(0)`。Rust 侧不终止进程：取消返回 `Ok(None)`，退出返回
//! `Err(CliExit { code: 0 })`，由二进制入口决定是否退出。
//!
//! 回调只把结果送进通道；`ui.stop()` 在驱动循环收到结果后执行（保证在回调返回后再停止，
//! 避免重入事件循环）。TUI 基于 `Rc`，本函数必须在 `LocalSet` 上运行。
//!
//! # 为什么需要
//! `--resume` 没带具体会话时，必须让用户从会话列表里挑一个。选择器本身是
//! `kiana-interactive` 的 `SessionSelectorComponent`（完整 TUI 屏幕）；本模块负责把它装进
//! 启动期 TUI、驱动事件循环，并把用户动作翻译成返回值。
//!
//! # 结局到返回值的映射
//! - 选中会话路径 → `Ok(Some(path))`；调用方随后打开该会话；
//! - 取消（`tui.select.cancel`，默认 Esc/Ctrl+C）→ `Ok(None)`；
//! - 退出键 → `Err(CliExit::new(0))`。注意 `SessionList.on_exit` 在当前组件里只被保存、
//!   不会被列表主动调用（该字段注释说明 TS 的列表同样从不调用），所以运行中通常先走到
//!   上一条取消路径；
//! - stdin EOF → `Err(CliExit::new(0))`，且**不**执行 `ui.stop()`
//!   （TS 里进程直接静默退出 0，只走进程级终端恢复）。
//!
//! # 并发与生命周期
//! - 回调通过 `Option<oneshot::Sender>` 的 `take()` 保证只结算一次；重复动作被静默忽略。
//! - 组件的 `requestRender` 要求 `Send + Sync`，但 TUI 是 `Rc` 结构，因此用
//!   `tokio::sync::mpsc` 把重绘请求送回驱动循环，而不是在回调里直接借用 TUI。
//! - `ui.stop()` 只在驱动循环拿到结局后调用：此时回调已经返回，不会二次借用 `RefCell`。
//!
//! 调用方是 `crate::app` 的 `--resume` 分支；它负责构造两个 loader、在返回后停止主题
//! watcher 并打开选中的会话文件。
//!
//! TS 源：`packages/coding-agent/src/cli/session-picker.ts`、`spec/ca-cli.md` §2.17。

use std::cell::RefCell;
use std::rc::Rc;

use kiana_config::settings::SettingsManager;
use kiana_interactive::components::session_selector::{
    SessionSelectorComponent, SessionSelectorOptions, SessionsLoader,
};
use kiana_tui::tui::{FocusTarget, Tui, TuiStopOptions};

use crate::cli::setup::{CliExit, CliResult};
use crate::cli::startup_ui::{
    DriveOutcome, create_startup_tui, drive_until_settled_with_render, start_startup_tui,
};

/// 选择器结果（TS 的 `onSelect` / `onCancel` / `onExit` 三条路径）。
enum SelectionOutcome {
    /// 选中会话文件路径。
    Selected(String),
    /// 取消（`null`）。
    Cancelled,
    /// 请求退出进程（`process.exit(0)`）。
    Exit,
}

/// `KeybindingsManager.create()` → kiana-tui 的 manager。
///
/// kiana-config 的 manager 没有暴露内部 manager，只能取出用户绑定重建一份（与
/// `startup_ui::create_startup_tui` 相同）；默认键位表由 kiana-tui 提供。
///
/// 这份 manager 用于设置 TUI 的进程级键位（`set_keybindings`）；`select_session` 另外
/// 创建一份 app manager 传给 [`SessionSelectorOptions`]，两者都基于磁盘上的用户绑定。
pub(crate) fn tui_keybindings_manager() -> kiana_tui::keybindings::KeybindingsManager {
    let app_manager =
        kiana_config::keybindings::KeybindingsManager::create(kiana_core::config::get_agent_dir());
    kiana_tui::keybindings::KeybindingsManager::new(
        kiana_config::keybindings::keybindings().clone(),
        app_manager.get_user_bindings(),
    )
}

/// `selectSession(currentSessionsLoader, allSessionsLoader, settingsManager)`。
///
/// # 参数
/// - `current_sessions_loader`：当前项目的会话加载器，组件默认展示这个 scope；
/// - `all_sessions_loader`：所有项目的会话加载器，用户切到 All scope 时使用；
/// - `settings_manager`：构造启动期 TUI（主题、键位、硬件光标等）所需的设置。
///
/// 两个 loader 由组件按需调用（参数是进度回调与取消信号），本函数不直接调用它们。
///
/// # 返回
/// - `Ok(Some(path))`：用户选中会话文件路径；
/// - `Ok(None)`：取消，调用方应正常继续（app 打印 `No session selected` 并以 0 退出）；
/// - `Err(CliExit::new(0))`：用户请求退出或 stdin 已关闭，调用方应结束进程。
///
/// # 前提
/// 必须在 `LocalSet` 上运行：函数会创建 TUI，并通过 `start_startup_tui` 在本地任务里
/// 查询终端颜色（`spawn_local`）。
///
/// # Panics
/// 与 `create_startup_tui` 相同：主题加载/注册失败时 panic（`FatalStartupError`），由二进制
/// 入口的 panic 钩子映射成 `Error: <message>` 并以退出码 1 结束。
pub async fn select_session(
    current_sessions_loader: SessionsLoader,
    all_sessions_loader: SessionsLoader,
    settings_manager: &SettingsManager,
) -> CliResult<Option<String>> {
    let ui = Rc::new(RefCell::new(create_startup_tui(settings_manager).await));

    // TS 在这里再设一次全局键位，并把同一个 manager 传给组件。
    let keybindings = tui_keybindings_manager();
    kiana_tui::keybindings::set_keybindings(keybindings);
    let app_keybindings =
        kiana_config::keybindings::KeybindingsManager::create(kiana_core::config::get_agent_dir());

    // 三个回调共享同一个 `Option<Sender>`：谁先触发谁取走 sender 并发送，后续动作看到
    // `None` 就静默返回——这就是 TS `resolved` 标志的等价物（只结算一次）。
    let (sender, mut receiver) = tokio::sync::oneshot::channel::<SelectionOutcome>();
    let settled: Rc<RefCell<Option<tokio::sync::oneshot::Sender<SelectionOutcome>>>> =
        Rc::new(RefCell::new(Some(sender)));
    // 组件要求 `requestRender` 是 `Send + Sync`，但 TUI 与组件都在 `Rc` 上；用无界通道
    // 把“需要重绘”这件事送回驱动循环，由循环在可安全借用 TUI 的时刻执行渲染。
    let (render_tx, mut render_rx) = tokio::sync::mpsc::unbounded_channel::<()>();

    let select_settled = Rc::clone(&settled);
    let on_select = Box::new(move |path: &str| {
        let mut guard = select_settled.borrow_mut();
        if let Some(sender) = guard.take() {
            let _ = sender.send(SelectionOutcome::Selected(path.to_string()));
        }
    });
    let cancel_settled = Rc::clone(&settled);
    let on_cancel = Box::new(move || {
        let mut guard = cancel_settled.borrow_mut();
        if let Some(sender) = guard.take() {
            let _ = sender.send(SelectionOutcome::Cancelled);
        }
    });
    let exit_settled = Rc::clone(&settled);
    let on_exit = Box::new(move || {
        let mut guard = exit_settled.borrow_mut();
        if let Some(sender) = guard.take() {
            let _ = sender.send(SelectionOutcome::Exit);
        }
    });
    let request_render: kiana_interactive::components::session_selector::RequestRenderFn = {
        let render_tx = render_tx.clone();
        std::sync::Arc::new(move || {
            let _ = render_tx.send(());
        })
    };

    let selector = SessionSelectorComponent::new(
        current_sessions_loader,
        all_sessions_loader,
        on_select,
        on_cancel,
        on_exit,
        request_render,
        SessionSelectorOptions {
            rename_session: None,
            show_rename_hint: Some(false),
            keybindings: Some(app_keybindings),
        },
        None,
    );

    {
        let mut ui = ui.borrow_mut();
        ui.add_child(Box::new(selector));
        // TS: `ui.setFocus(selector.getSessionList())`；组件的 `set_focused` 会转发到列表。
        ui.set_focus(Some(FocusTarget::Component { index: 0 }));
    }
    start_startup_tui(&ui, settings_manager);

    let outcome = drive_until_settled_with_render(&ui, &mut receiver, &mut render_rx).await;
    match outcome {
        // stdin 关闭：TS 的 promise 永不 resolve，Node 事件循环耗尽后静默退出 0；
        // 不走 `ui.stop()`（只有进程 exit 处理器写终端恢复序列）。
        DriveOutcome::StdinEof => Err(CliExit::new(0)),
        DriveOutcome::Settled(outcome) => {
            ui.borrow_mut().stop(TuiStopOptions::default());
            // 通道在发送前关闭（`Settled(None)`）按取消处理：TS 里等价于悬空 promise
            // 的取消路径。
            match outcome.unwrap_or(SelectionOutcome::Cancelled) {
                SelectionOutcome::Selected(path) => Ok(Some(path)),
                SelectionOutcome::Cancelled => Ok(None),
                SelectionOutcome::Exit => Err(CliExit::new(0)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiana_interactive::components::session_selector::SessionLoaderFuture;
    use kiana_session_core::session_manager::SessionInfo;

    fn empty_loader() -> SessionsLoader {
        std::sync::Arc::new(|_progress, _signal| -> SessionLoaderFuture {
            Box::pin(async { Ok(Vec::<SessionInfo>::new()) })
        })
    }

    /// 手动 TTY 测试：真实打开会话选择器（Esc / Ctrl+C 都命中 `tui.select.cancel`，
    /// 因此走取消路径；进程随后由调用方以退出码 0 结束）。
    #[test]
    #[ignore = "manual TTY test: opens the session selector"]
    fn select_session_manually() {
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
                kiana_config::settings::SettingsManagerCreateOptions {
                    project_trusted: Some(false),
                },
            );
            let result = select_session(empty_loader(), empty_loader(), &settings_manager).await;
            assert!(result.is_ok() || result.is_err());
        });
    }
}
