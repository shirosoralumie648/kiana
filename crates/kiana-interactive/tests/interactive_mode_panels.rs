//! 面板命令路由的集成测试（`tui-panels-design.md` §9.1）。
//!
//! 覆盖 `/permissions`、`/tasks`、`/mcp` 三个入口的挂载与用法错误，并锁定
//! `/mcp <subcommand>` 仍走消息路径（由内建扩展处理），不会被面板路由抢走。
//! 这些用例只断言“选择器是否挂载/编辑器是否清空/用法文本是否出现”，不依赖面板内部
//! 渲染细节（那由 `permissions_dialog.rs`、`components_tasks_dialog.rs`、
//! `components_mcp_dialog.rs` 锁定）。

#![allow(
    clippy::await_holding_lock,
    reason = "主题初始化是进程级状态：同文件的用例必须串行（TS 的 beforeAll(initTheme) 等价）"
)]

mod common;

use std::sync::{Mutex, MutexGuard};

use kiana_config::settings::TuiMode;
use kiana_interactive::interactive_mode::{InteractiveMode, InteractiveModeOptions};

static THEME_LOCK: Mutex<()> = Mutex::new(());

fn theme() -> MutexGuard<'static, ()> {
    let guard = THEME_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    kiana_theme::init_theme(Some("dark"), false);
    guard
}

async fn fixture() -> InteractiveMode {
    unsafe { std::env::set_var("KIANA_OFFLINE", "1") };
    let host = common::runtime_host(common::RuntimeHostOptions::new()).await;
    let common::RuntimeHost {
        runtime,
        faux,
        temp_dir,
        cwd,
        engine,
        ..
    } = host;
    // 让夹具的生命周期覆盖 mode：把依赖挂到静态之外不可行，这里泄露测试专用的
    // runtime 句柄，避免 `InteractiveMode` 持有悬垂引用（测试进程很快退出）。
    std::mem::forget(faux);
    std::mem::forget(temp_dir);
    std::mem::forget(engine);
    std::mem::forget(cwd);
    InteractiveMode::new(
        runtime,
        InteractiveModeOptions {
            terminal: Some(Box::new(common::TestTerminal::new())),
            tui_mode: Some(TuiMode::Regular),
            ..Default::default()
        },
    )
}

#[tokio::test(flavor = "current_thread")]
async fn permissions_command_mounts_the_panel_and_clears_the_editor() {
    let _theme = theme();
    common::run_local(async {
        let mut mode = fixture().await;
        mode.test_set_editor_text("/permissions");
        mode.test_dispatch_submit("/permissions");
        assert_eq!(mode.test_editor_text(), "");
        assert!(mode.test_has_active_selector());
        assert!(mode.test_selector_child_address().is_some());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn permissions_with_arguments_reports_usage_without_mounting() {
    let _theme = theme();
    common::run_local(async {
        let mut mode = fixture().await;
        mode.test_dispatch_submit("/permissions allow");
        assert!(!mode.test_has_active_selector());
        assert!(mode.chat_render_text().contains("Usage: /permissions"));
        assert_eq!(mode.test_pending_user_input_count(), 0);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn tasks_command_mounts_the_panel_after_loading_the_snapshot() {
    let _theme = theme();
    common::run_local(async {
        let mut mode = fixture().await;
        mode.test_set_editor_text("/tasks");
        mode.test_dispatch_submit("/tasks");
        assert_eq!(mode.test_editor_text(), "");
        common::yield_tasks(20).await;
        assert!(mode.test_has_active_selector());
        assert!(mode.test_selector_child_address().is_some());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn tasks_with_arguments_reports_usage_without_mounting() {
    let _theme = theme();
    common::run_local(async {
        let mut mode = fixture().await;
        mode.test_dispatch_submit("/tasks all");
        common::yield_tasks(5).await;
        assert!(!mode.test_has_active_selector());
        assert!(mode.chat_render_text().contains("Usage: /tasks"));
        assert_eq!(mode.test_pending_user_input_count(), 0);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn mcp_command_mounts_the_panel() {
    let _theme = theme();
    common::run_local(async {
        let mut mode = fixture().await;
        mode.test_set_editor_text("/mcp");
        mode.test_dispatch_submit("/mcp");
        assert_eq!(mode.test_editor_text(), "");
        assert!(mode.test_has_active_selector());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn mcp_subcommands_still_go_to_the_extension_path() {
    let _theme = theme();
    common::run_local(async {
        let mut mode = fixture().await;
        mode.test_dispatch_submit("/mcp login");
        assert!(!mode.test_has_active_selector());
        // 普通消息路径：没有等待中的 getUserInput 回调时进入 pending 队列（随后由
        // `session.prompt` 交给扩展 runner）。
        assert_eq!(mode.test_pending_user_input_count(), 1);
    })
    .await;
}
