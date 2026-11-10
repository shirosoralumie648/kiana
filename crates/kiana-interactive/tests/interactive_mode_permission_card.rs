//! 第 12 周：TUI 的 `permission_card`（`ApprovalCard` + `ModalArbiter` 队列）。
//!
//! 覆盖 audit 发现 10：
//! * `InteractiveUiContext` 真正渲染 `ApprovalCard`（工具名 + 命令 + 建议 + 引擎原因），
//!   而不是退化成通用 `select`；
//! * 多个并发审批共享一个对话框，`ModalArbiter` 显示 `Approval i/N` 并按 FIFO 完成；
//! * 危险动作默认焦点 Reject，Enter 不能批准，必须 Ctrl+Enter 显式确认；
//! * Escape / UI 关闭按 fail-closed 返回 Reject。

#![allow(
    clippy::await_holding_refcell_ref,
    clippy::await_holding_lock,
    reason = "单线程 LocalSet 测试：扩展工厂没有 Send/Sync 约束，且不存在并发借用者"
)]

mod common;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};

use kiana_config::settings::TuiMode;
use kiana_ext_api::ui::{
    ExtensionUiContext, PermissionCardChoice, PermissionCardRequest, PermissionScopeKind,
};
use kiana_interactive::interactive_mode::{InteractiveMode, InteractiveModeOptions};

static THEME_LOCK: Mutex<()> = Mutex::new(());

fn theme() -> MutexGuard<'static, ()> {
    let guard = THEME_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    kiana_theme::init_theme(Some("dark"), false);
    guard
}

struct Fixture {
    mode: Rc<RefCell<InteractiveMode>>,
    _faux: kiana_ai::providers::faux::FauxCore,
    _temp: tempfile::TempDir,
    _engine: common::TestEngine,
}

async fn fixture() -> Fixture {
    unsafe { std::env::set_var("KIANA_OFFLINE", "1") };
    let host = common::runtime_host(common::RuntimeHostOptions::new()).await;
    let common::RuntimeHost {
        runtime,
        faux,
        temp_dir,
        engine,
        ..
    } = host;
    let mode = InteractiveMode::new(
        runtime,
        InteractiveModeOptions {
            terminal: Some(Box::new(common::TestTerminal::new())),
            tui_mode: Some(TuiMode::Regular),
            ..Default::default()
        },
    );
    Fixture {
        mode: Rc::new(RefCell::new(mode)),
        _faux: faux,
        _temp: temp_dir,
        _engine: engine,
    }
}

fn context(mode: &Rc<RefCell<InteractiveMode>>) -> Arc<dyn ExtensionUiContext> {
    mode.borrow_mut().test_extension_ui_context()
}

fn bash_card(command: &str, dangerous: bool) -> PermissionCardRequest {
    PermissionCardRequest {
        tool_name: "Bash".to_string(),
        args: serde_json::json!({ "command": command }),
        cwd: "/tmp/project".to_string(),
        decision_reason: "default mode asks for Bash".to_string(),
        always_scopes: vec![PermissionScopeKind::Session, PermissionScopeKind::Project],
        dangerous,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn renders_an_approval_card_and_allows_once() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        let ui = context(&fixture.mode);

        let ui_call = Arc::clone(&ui);
        let pending = tokio::task::spawn_local(async move {
            ui_call
                .permission_card(bash_card("echo hello", false))
                .await
        });
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 1).await;

        let rendered = fixture.mode.borrow().test_dialog_render(0);
        assert!(
            rendered.contains("Permission required: Run shell command (Bash)"),
            "{rendered}"
        );
        assert!(rendered.contains("echo hello"), "{rendered}");
        assert!(rendered.contains("Approval 1/1"), "{rendered}");
        assert!(
            rendered.contains("Reason: default mode asks for Bash"),
            "{rendered}"
        );
        assert!(rendered.contains("> 1. Allow once"), "{rendered}");
        assert!(
            rendered.contains("Always allow (this session)"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Always allow (this project)"),
            "{rendered}"
        );
        assert!(rendered.contains("Reject"), "{rendered}");

        // 默认焦点是 Allow once（非危险动作），Enter 批准一次。
        fixture.mode.borrow_mut().test_send_to_top_dialog("\r");
        assert_eq!(
            pending.await.expect("permission card result"),
            Some(PermissionCardChoice::AllowOnce)
        );
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 0).await;
        assert_eq!(fixture.mode.borrow().test_editor_slot_child_count(), 1);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn permission_card_uses_the_configured_ui_locale() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        {
            let mut mode = fixture.mode.borrow_mut();
            mode.settings_manager().set_ui_locale("zh");
            mode.test_apply_ui_locale("zh");
        }
        let ui = context(&fixture.mode);

        let ui_call = Arc::clone(&ui);
        let pending = tokio::task::spawn_local(async move {
            ui_call.permission_card(bash_card("echo 你好", false)).await
        });
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 1).await;

        let rendered = fixture.mode.borrow().test_dialog_render(0);
        assert!(
            rendered.contains("需要权限: Run shell command (Bash)"),
            "{rendered}"
        );
        assert!(rendered.contains("审批 1/1"), "{rendered}");
        assert!(rendered.contains("只允许一次"), "{rendered}");
        assert!(rendered.contains("拒绝"), "{rendered}");
        assert!(!rendered.contains("Permission required"), "{rendered}");

        fixture.mode.borrow_mut().test_send_to_top_dialog("\r");
        assert_eq!(
            pending.await.expect("permission card result"),
            Some(PermissionCardChoice::AllowOnce)
        );
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 0).await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn dangerous_card_defaults_to_reject_and_enter_cannot_approve() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        let ui = context(&fixture.mode);

        let ui_call = Arc::clone(&ui);
        let pending = tokio::task::spawn_local(async move {
            ui_call
                .permission_card(bash_card("rm -rf /tmp/kiana-danger", true))
                .await
        });
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 1).await;

        let rendered = fixture.mode.borrow().test_dialog_render(0);
        assert!(
            rendered.contains("Dangerous action: default selection is Reject"),
            "{rendered}"
        );
        assert!(rendered.contains("> 4. Reject"), "{rendered}");

        // 上移到 Allow once；危险动作上的 Enter 必须被忽略。
        for _ in 0..3 {
            fixture.mode.borrow_mut().test_send_to_top_dialog("\x1b[A");
        }
        let rendered = fixture.mode.borrow().test_dialog_render(0);
        assert!(rendered.contains("> 1. Allow once"), "{rendered}");
        fixture.mode.borrow_mut().test_send_to_top_dialog("\r");
        assert_eq!(fixture.mode.borrow().test_dialog_count(), 1);
        assert!(!pending.is_finished());

        // Ctrl+Enter 是显式确认键。
        fixture
            .mode
            .borrow_mut()
            .test_send_to_top_dialog("\x1b[27;5;13~");
        assert_eq!(
            pending.await.expect("permission card result"),
            Some(PermissionCardChoice::AllowOnce)
        );
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 0).await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_permission_cards_share_one_queued_dialog() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        let ui = context(&fixture.mode);

        let first_ui = Arc::clone(&ui);
        let first = tokio::task::spawn_local(async move {
            first_ui
                .permission_card(bash_card("echo first", false))
                .await
        });
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 1).await;

        let second_ui = Arc::clone(&ui);
        let second = tokio::task::spawn_local(async move {
            second_ui
                .permission_card(bash_card("echo second", false))
                .await
        });
        // 第二张卡排队，而不是叠加第二个对话框。
        common::wait_until(|| {
            fixture
                .mode
                .borrow()
                .test_dialog_render(0)
                .contains("Approval 1/2")
        })
        .await;
        assert_eq!(fixture.mode.borrow().test_dialog_count(), 1);
        let rendered = fixture.mode.borrow().test_dialog_render(0);
        assert!(rendered.contains("echo first"), "{rendered}");
        assert!(!rendered.contains("echo second"), "{rendered}");

        // 完成第一张：第二张自动提升，队列仍只有一个对话框。
        fixture.mode.borrow_mut().test_send_to_top_dialog("\r");
        assert_eq!(
            first.await.expect("first result"),
            Some(PermissionCardChoice::AllowOnce)
        );
        let rendered = fixture.mode.borrow().test_dialog_render(0);
        assert!(rendered.contains("Approval 1/1"), "{rendered}");
        assert!(rendered.contains("echo second"), "{rendered}");
        assert_eq!(fixture.mode.borrow().test_dialog_count(), 1);

        fixture.mode.borrow_mut().test_send_to_top_dialog("\r");
        assert_eq!(
            second.await.expect("second result"),
            Some(PermissionCardChoice::AllowOnce)
        );
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 0).await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn escape_rejects_the_card_and_closes_the_dialog() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        let ui = context(&fixture.mode);

        let ui_call = Arc::clone(&ui);
        let pending = tokio::task::spawn_local(async move {
            ui_call
                .permission_card(bash_card("echo escape", false))
                .await
        });
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 1).await;

        fixture.mode.borrow_mut().test_send_to_top_dialog("\x1b");
        assert_eq!(
            pending.await.expect("permission card result"),
            Some(PermissionCardChoice::Reject {
                reason: Some("Approval was cancelled.".to_string())
            })
        );
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 0).await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn always_allow_returns_the_selected_scope() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        let ui = context(&fixture.mode);

        let ui_call = Arc::clone(&ui);
        let pending = tokio::task::spawn_local(async move {
            ui_call
                .permission_card(bash_card("echo scope", false))
                .await
        });
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 1).await;

        // Allow once → Always allow (this session) → Always allow (this project) → Reject。
        fixture.mode.borrow_mut().test_send_to_top_dialog("\x1b[B");
        fixture.mode.borrow_mut().test_send_to_top_dialog("\r");
        assert_eq!(
            pending.await.expect("permission card result"),
            Some(PermissionCardChoice::AllowAlways(
                PermissionScopeKind::Session
            ))
        );
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 0).await;
    })
    .await;
}
