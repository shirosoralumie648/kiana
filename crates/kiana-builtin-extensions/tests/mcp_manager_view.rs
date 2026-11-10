//! `packages/coding-agent/test/mcp-manager-view.test.ts`：管理器状态页的取消键语义。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use kiana_builtin_extensions::mcp::ui::{McpManagerView, McpUi};
use kiana_config::keybindings::KeybindingsManager;
use kiana_core::ansi::strip_ansi;
use kiana_theme::theme::{Theme, get_theme_by_name, init_theme};
use kiana_tui::tui::Component;

const ESCAPE: &str = "\x1b";

/// 初始化暗色主题（TS 的 `beforeAll(() => initTheme("dark"))`）。
fn theme() -> Arc<Theme> {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| init_theme(Some("dark"), false));
    get_theme_by_name("dark").expect("builtin dark theme")
}

fn view() -> McpManagerView {
    McpManagerView::new(
        Rc::new(|| {}),
        theme(),
        Arc::new(KeybindingsManager::new(Default::default(), None)),
        None,
    )
}

fn rendered(view: &mut McpManagerView) -> String {
    strip_ansi(&view.render(80).join("\n"))
}

#[test]
fn cancels_the_running_operation_with_the_cancel_key() {
    let mut view = view();
    let cancelled = Rc::new(RefCell::new(0usize));
    let on_cancel: Rc<dyn Fn()> = {
        let cancelled = Rc::clone(&cancelled);
        Rc::new(move || *cancelled.borrow_mut() += 1)
    };
    view.status(
        "Sign in to issues",
        "Contacting the authorization server…",
        Some(on_cancel),
    );
    assert!(rendered(&mut view).contains("cancel"));
    view.handle_input(ESCAPE);
    assert_eq!(*cancelled.borrow(), 1);
}

#[test]
fn ignores_the_cancel_key_for_operations_that_cannot_be_cancelled() {
    let mut view = view();
    view.status("Sign in to issues", "Connecting…", None);
    assert!(!rendered(&mut view).contains("cancel"));
    // 没有 input handler：按 Esc 不应 panic，也不应触发任何动作。
    view.handle_input(ESCAPE);
    assert!(!rendered(&mut view).contains("cancel"));
}
