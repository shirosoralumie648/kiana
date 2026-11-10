//! `packages/coding-agent/test/auth-url-copy.test.ts` 的
//! `MCP sign-in screen copies the authorization URL`。
//!
//! 同文件的另外两个测试针对 `LoginDialogComponent`（`kiana-interactive`），不属本单元。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use kiana_builtin_extensions::mcp::ui::{McpManagerView, McpUi, UrlCopier};
use kiana_config::keybindings::KeybindingsManager;
use kiana_core::ansi::strip_ansi;
use kiana_ext_api::LocalBoxFuture;
use kiana_theme::theme::{Theme, get_theme_by_name, init_theme};
use kiana_tui::tui::Component;
use tokio_util::sync::CancellationToken;

const CTRL_X: &str = "\x18";

fn theme() -> Arc<Theme> {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| init_theme(Some("dark"), false));
    get_theme_by_name("dark").expect("builtin dark theme")
}

/// 安装包含 `app.message.copy -> ["ctrl+x"]` 的全局键位表（`key_hint` 读全局表）。
///
/// `kiana_tui` 的全局键位是 thread-local：每个测试线程都要安装一次（不能用 `OnceLock`，
/// 否则第二个测试线程会拿到默认键位表）。
fn install_keybindings() {
    kiana_tui::keybindings::set_keybindings(kiana_tui::keybindings::KeybindingsManager::new(
        kiana_config::keybindings::keybindings().clone(),
        kiana_tui::keybindings::KeybindingsConfig::new(),
    ));
}

fn rendered(view: &mut McpManagerView) -> String {
    strip_ansi(&view.render(80).join("\n"))
}

#[test]
fn mcp_sign_in_screen_copies_the_authorization_url() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let local = tokio::task::LocalSet::new();
    runtime.block_on(local.run_until(async {
        install_keybindings();
        let url = format!("https://auth.example.invalid/authorize?{}", "x".repeat(300));
        let copied: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let copier: UrlCopier = {
            let copied = Rc::clone(&copied);
            Rc::new(move |url: String| {
                copied.borrow_mut().push(url);
                Box::pin(async { Ok(()) }) as LocalBoxFuture<'static, Result<(), String>>
            })
        };
        let mut view = McpManagerView::new(
            Rc::new(|| {}),
            theme(),
            Arc::new(KeybindingsManager::new(Default::default(), None)),
            Some(copier),
        );
        let _pending = view.redirect_url("Sign in to issues", &url, CancellationToken::new());
        assert!(rendered(&mut view).contains("ctrl+x to copy"));
        view.handle_input(CTRL_X);
        let mut copied_hint = false;
        for _ in 0..200 {
            tokio::task::yield_now().await;
            if rendered(&mut view).contains("Copied URL to clipboard") {
                copied_hint = true;
                break;
            }
        }
        assert!(copied_hint, "copy hint not shown");
        assert_eq!(copied.borrow().as_slice(), std::slice::from_ref(&url));
    }));
}

/// 复制进行中时授权 URL 必须仍然渲染（TS 保持 `AuthUrlComponent` 挂载，只改提示行）。
#[test]
fn mcp_sign_in_screen_keeps_the_url_while_the_copy_is_in_flight() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let local = tokio::task::LocalSet::new();
    runtime.block_on(local.run_until(async {
        install_keybindings();
        let url = "https://auth.example.invalid/authorize?state=abc";
        let sender = Rc::new(RefCell::new(None));
        let receiver = Rc::new(RefCell::new(None));
        let (tx, rx) = futures::channel::oneshot::channel::<()>();
        *sender.borrow_mut() = Some(tx);
        *receiver.borrow_mut() = Some(rx);
        let copier: UrlCopier = {
            let receiver = Rc::clone(&receiver);
            Rc::new(move |_url: String| {
                let receiver = receiver.borrow_mut().take();
                Box::pin(async move {
                    if let Some(receiver) = receiver {
                        let _ = receiver.await;
                    }
                    Ok(())
                }) as LocalBoxFuture<'static, Result<(), String>>
            })
        };
        let mut view = McpManagerView::new(
            Rc::new(|| {}),
            theme(),
            Arc::new(KeybindingsManager::new(Default::default(), None)),
            Some(copier),
        );
        let _pending = view.redirect_url("Sign in to issues", url, CancellationToken::new());
        assert!(rendered(&mut view).contains(url));
        view.handle_input(CTRL_X);
        // 让复制任务运行到等待剪贴板实现的位置。
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        let pending_render = rendered(&mut view);
        assert!(
            pending_render.contains(url),
            "authorization URL must stay visible while the copy is in flight"
        );
        assert!(!pending_render.contains("Copied URL to clipboard"));
        sender
            .borrow_mut()
            .take()
            .expect("sender")
            .send(())
            .expect("receiver dropped");
        let mut copied_hint = false;
        for _ in 0..200 {
            tokio::task::yield_now().await;
            if rendered(&mut view).contains("Copied URL to clipboard") {
                copied_hint = true;
                break;
            }
        }
        assert!(copied_hint, "copy hint not shown");
        let done_render = rendered(&mut view);
        assert!(done_render.contains(url));
        assert!(done_render.contains("Copied URL to clipboard"));
    }));
}
