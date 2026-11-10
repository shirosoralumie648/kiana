//! `bug-report.test.ts` 的移植。

use std::cell::RefCell;
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};

use kiana_config::keybindings::{KeybindingsConfig, KeybindingsManager as AppKeybindingsManager};
use kiana_interactive::bug_report::{BugReportContext, report_bug};
use kiana_interactive::components::extension_editor::ExtensionEditorComponent;
use kiana_interactive::components::extension_selector::ExtensionSelectorComponent;
use kiana_interactive::tui_renderer::SharedComponentProxy;
use kiana_resources::bug_report::{redact_json_value, redact_url};
use kiana_session::agent_session::AgentSession;
use kiana_session_core::session_manager::SessionManagerError;
use kiana_theme::init_theme;
use kiana_tui::components::editor::EditorTui;
use kiana_tui::components::text_component::Text;
use kiana_tui::keybindings::{
    KeybindingsConfig as TuiKeybindingsConfig, KeybindingsManager, set_keybindings,
};
use kiana_tui::layout_node::SharedComponent;
use kiana_tui::terminal_colors::Terminal;
use kiana_tui::tui::{Component, Container, Tui};
use serde_json::json;

use crate::common::{
    FauxSessionOptions, TestTerminal, faux_session, main_screen, run_local, shared, wait_until,
};

mod common;

/// 进程级状态（cwd / 环境变量）在同一个测试进程内必须串行化。
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn test_guard() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// cwd 守卫（panic 时也恢复）。
struct CwdGuard(PathBuf);

impl CwdGuard {
    fn enter(path: &std::path::Path) -> Self {
        let previous = std::env::current_dir().expect("capture cwd");
        std::env::set_current_dir(path).expect("enter readonly dir");
        CwdGuard(previous)
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.0);
    }
}

/// 环境变量守卫（panic 时也恢复）。
struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &std::ffi::OsStr) -> Self {
        let previous = std::env::var_os(key);
        unsafe { std::env::set_var(key, value) };
        EnvGuard { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

type Messages = Rc<RefCell<Vec<String>>>;

/// `/bug` 流程的测试夹具（与 TS 的 `BugReportContext` 相同的编辑器槽位）。
struct BugHarness {
    context: BugReportContext,
    editor_container: Rc<RefCell<Container>>,
    editor: SharedComponent,
}

fn bug_harness(session: AgentSession, statuses: Messages, errors: Messages) -> BugHarness {
    let keybindings = install_keybindings();
    let ui: Rc<RefCell<dyn Tui>> = Rc::new(RefCell::new(main_screen(TestTerminal::new())));
    let editor_container = Rc::new(RefCell::new(Container::new()));
    let editor: SharedComponent = shared(Text::new("editor"));
    let context = BugReportContext {
        session,
        ui,
        editor_container: Rc::clone(&editor_container),
        editor: Rc::clone(&editor),
        editor_tui: editor_tui(),
        keybindings,
        show_status: {
            let statuses = Rc::clone(&statuses);
            Rc::new(move |message: &str| statuses.borrow_mut().push(message.to_string()))
        },
        show_error: {
            let errors = Rc::clone(&errors);
            Rc::new(move |message: &str| errors.borrow_mut().push(message.to_string()))
        },
    };
    BugHarness {
        context,
        editor_container,
        editor,
    }
}

/// 第一屏：提交（可为空的）描述。
async fn submit_description(editor_container: &Rc<RefCell<Container>>) {
    wait_until(|| has_overlay::<ExtensionEditorComponent>(editor_container)).await;
    take_overlay(editor_container).handle_input("\r");
}

/// 选择器第二项（`"No"` / `"Export as Zip"` 前的 j 次数由调用方给出）。
async fn select_option(editor_container: &Rc<RefCell<Container>>, downs: usize) {
    wait_until(|| has_overlay::<ExtensionSelectorComponent>(editor_container)).await;
    let mut selector = take_overlay(editor_container);
    for _ in 0..downs {
        selector.handle_input("j");
    }
    selector.handle_input("\r");
}

fn new_messages() -> Messages {
    Rc::new(RefCell::new(Vec::new()))
}

fn theme() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| init_theme(Some("dark"), false));
}

fn install_keybindings() -> Rc<AppKeybindingsManager> {
    let keybindings = Rc::new(AppKeybindingsManager::new(KeybindingsConfig::new(), None));
    set_keybindings(KeybindingsManager::new(
        kiana_config::keybindings::keybindings().clone(),
        TuiKeybindingsConfig::new(),
    ));
    keybindings
}

fn editor_tui() -> EditorTui {
    let terminal: Rc<RefCell<Box<dyn Terminal>>> =
        Rc::new(RefCell::new(Box::new(TestTerminal::with_size(80, 24))));
    EditorTui::from_terminal(terminal)
}

fn take_overlay(container: &Rc<RefCell<Container>>) -> Box<dyn Component> {
    container
        .borrow_mut()
        .remove_child_at(0)
        .expect("an overlay is mounted")
}

/// `ExtensionEditorComponent` 没有 `as_any_mut`（组件单元只实现了 `as_any`），因此类型
/// 判定走不可变向下转型；输入直接转发给 `&mut dyn Component`。
fn has_overlay<T: 'static>(container: &Rc<RefCell<Container>>) -> bool {
    let mut container = container.borrow_mut();
    container
        .children
        .first_mut()
        .and_then(|child| child.as_any())
        .and_then(|any| any.downcast_ref::<T>())
        .is_some()
}

fn is_shared_editor(container: &Rc<RefCell<Container>>, editor: &SharedComponent) -> bool {
    let mut container = container.borrow_mut();
    container
        .children
        .first_mut()
        .and_then(|child| child.as_any())
        .and_then(|any| any.downcast_ref::<SharedComponentProxy>())
        .is_some_and(|proxy| Rc::ptr_eq(proxy.component(), editor))
}

fn strip(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|line| kiana_tui::text::strip_terminal_sequences(line))
        .collect()
}

#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::await_holding_lock,
    reason = "cwd / 环境变量是进程级状态：同文件的用例必须串行"
)]
async fn preserves_line_breaks_in_pasted_descriptions() {
    theme();
    let _guard = test_guard();
    let keybindings = install_keybindings();
    let statuses: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let errors: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

    run_local(async move {
        let fixture = faux_session(FauxSessionOptions::default()).await;
        let ui: Rc<RefCell<dyn Tui>> = Rc::new(RefCell::new(main_screen(TestTerminal::new())));
        let editor_container = Rc::new(RefCell::new(Container::new()));
        let editor: SharedComponent = shared(Text::new("editor"));
        let context = BugReportContext {
            session: fixture.session.clone(),
            ui: Rc::clone(&ui),
            editor_container: Rc::clone(&editor_container),
            editor: Rc::clone(&editor),
            editor_tui: editor_tui(),
            keybindings: Rc::clone(&keybindings),
            show_status: {
                let statuses = Rc::clone(&statuses);
                Rc::new(move |message: &str| statuses.borrow_mut().push(message.to_string()))
            },
            show_error: {
                let errors = Rc::clone(&errors);
                Rc::new(move |message: &str| errors.borrow_mut().push(message.to_string()))
            },
        };
        let handle = tokio::task::spawn_local(report_bug(context, None));

        // 1. 描述输入框。
        wait_until(|| has_overlay::<ExtensionEditorComponent>(&editor_container)).await;
        let mut description = take_overlay(&editor_container);
        assert!(
            description
                .as_any()
                .and_then(|any| any.downcast_ref::<ExtensionEditorComponent>())
                .is_some(),
            "first overlay must be the description editor"
        );
        description.handle_input(
            "\x1b[200~Request failed\r\n  ↳ kiana exiting...\r\nstack trace\x1b[201~",
        );
        description.handle_input("\r");
        assert!(
            is_shared_editor(&editor_container, &editor),
            "the editor is restored synchronously after submit"
        );

        // 2. transcript 选择器。
        wait_until(|| has_overlay::<ExtensionSelectorComponent>(&editor_container)).await;
        let mut transcript = take_overlay(&editor_container);
        let lines = strip(&transcript.render(120));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Include the session transcript?")),
            "transcript selector title missing: {lines:?}"
        );
        transcript.handle_input("j");
        transcript.handle_input("\r");
        assert!(is_shared_editor(&editor_container, &editor));

        // 3. 摘要选择器。
        wait_until(|| has_overlay::<ExtensionSelectorComponent>(&editor_container)).await;
        let mut summary = take_overlay(&editor_container);
        let lines = strip(&summary.render(120));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Attach a summary written by")),
            "summary selector title missing: {lines:?}"
        );
        summary.handle_input("j");
        summary.handle_input("\r");
        assert!(is_shared_editor(&editor_container, &editor));

        // 4. 交付方式选择器：描述里保留多行粘贴内容。
        wait_until(|| has_overlay::<ExtensionSelectorComponent>(&editor_container)).await;
        let mut delivery = take_overlay(&editor_container);
        let delivery_lines = strip(&delivery.render(120));
        assert!(
            delivery_lines
                .iter()
                .any(|line| line.contains("Description: Request failed")),
            "description missing: {delivery_lines:?}"
        );
        assert!(
            delivery_lines
                .iter()
                .any(|line| line.contains("↳ kiana exiting...")),
            "second line missing: {delivery_lines:?}"
        );
        assert!(
            delivery_lines
                .iter()
                .any(|line| line.contains("stack trace")),
            "third line missing: {delivery_lines:?}"
        );
        delivery.handle_input("j");
        delivery.handle_input("j");
        delivery.handle_input("\r");
        assert!(is_shared_editor(&editor_container, &editor));

        handle
            .await
            .expect("report bug task")
            .expect("report bug completes");
        assert_eq!(*statuses.borrow(), vec!["Bug report cancelled".to_string()]);
        assert!(errors.borrow().is_empty(), "errors: {:?}", errors.borrow());
    })
    .await;
}

#[test]
fn removes_url_credentials_and_secret_query_parameters() {
    let _guard = test_guard();
    assert_eq!(
        redact_url("https://user:pass@proxy.example.com:8080/"),
        "https://proxy.example.com:8080/"
    );
    assert_eq!(
        redact_url("git:https://pat@github.com/org/repo"),
        "git:https://github.com/org/repo"
    );
    assert_eq!(
        redact_url("https://api.example/v1?api-key=abc&model=x"),
        "https://api.example/v1?api-key=%3Credacted%3E&model=x"
    );
}

#[test]
fn redacts_nested_secret_values_without_hiding_token_counts() {
    let _guard = test_guard();
    assert_eq!(
        redact_json_value(&json!({
            "apiKey": "sk-123",
            "headers": { "Authorization": "Bearer x", "X-Trace": "1" },
            "compaction": { "reserveTokens": 16_384, "keepRecentTokens": 20_000 },
            "baseUrl": "https://me:secret@example.com/"
        })),
        json!({
            "apiKey": "<redacted>",
            "headers": { "Authorization": "<redacted>", "X-Trace": "1" },
            "compaction": { "reserveTokens": 16_384, "keepRecentTokens": 20_000 },
            "baseUrl": "https://example.com/"
        })
    );
}

/// 回归（review finding）：zip 导出写盘失败时显示 Node fs 的异常文案
/// （`EACCES: permission denied, open '<path>'`），而不是 Rust `io::Error` 的 Display。
#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::await_holding_lock,
    reason = "cwd / 环境变量是进程级状态：同文件的用例必须串行"
)]
async fn reports_node_style_write_errors_for_the_zip_export() {
    theme();
    let _guard = test_guard();
    let readonly = tempfile::tempdir().expect("readonly dir");
    let mut permissions = std::fs::metadata(readonly.path())
        .expect("metadata")
        .permissions();
    permissions.set_mode(0o555);
    std::fs::set_permissions(readonly.path(), permissions).expect("chmod readonly");
    let readonly_path = readonly.path().to_path_buf();

    let statuses = new_messages();
    let errors = new_messages();
    let statuses_in = Rc::clone(&statuses);
    let errors_in = Rc::clone(&errors);
    run_local(async move {
        let fixture = faux_session(FauxSessionOptions::default()).await;
        let harness = bug_harness(fixture.session.clone(), statuses_in, errors_in);
        let _cwd = CwdGuard::enter(&readonly_path);
        let handle = tokio::task::spawn_local(report_bug(harness.context, None));

        submit_description(&harness.editor_container).await;
        // transcript: "No"（第二项）→ 出现摘要选择器。
        select_option(&harness.editor_container, 1).await;
        // summary: "No"（第二项）。
        select_option(&harness.editor_container, 1).await;
        // delivery: "Export as Zip"（第二项）。
        select_option(&harness.editor_container, 1).await;

        handle
            .await
            .expect("report bug task")
            .expect("report_bug resolves Ok");
        assert!(is_shared_editor(&harness.editor_container, &harness.editor));
    })
    .await;

    // 恢复权限，TempDir 才能递归删除。
    let mut permissions = std::fs::metadata(readonly.path())
        .expect("metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(readonly.path(), permissions).expect("chmod restore");

    let errors = errors.borrow();
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    let message = &errors[0];
    let prefix = "Failed to write bug report: EACCES: permission denied, open '";
    assert!(message.starts_with(prefix), "message: {message}");
    assert!(message.contains("/kiana-bug-report-"), "message: {message}");
    assert!(message.ends_with(".zip'"), "message: {message}");
    assert!(statuses.borrow().is_empty());
}

/// 回归（review finding）：`KIANA_RADIUS_GATEWAY` 无法解析时 `new URL(...)` 抛错，
/// `reportBug` reject（不显示带空 host 的交付对话框）。
#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::await_holding_lock,
    reason = "cwd / 环境变量是进程级状态：同文件的用例必须串行"
)]
async fn rejects_the_bug_flow_when_the_radius_gateway_url_is_malformed() {
    theme();
    let _guard = test_guard();
    let _env = EnvGuard::set("KIANA_RADIUS_GATEWAY", std::ffi::OsStr::new("https://"));

    let statuses = new_messages();
    let errors = new_messages();
    let statuses_in = Rc::clone(&statuses);
    let errors_in = Rc::clone(&errors);
    let result = run_local(async move {
        let fixture = faux_session(FauxSessionOptions::default()).await;
        let harness = bug_harness(fixture.session.clone(), statuses_in, errors_in);
        let handle = tokio::task::spawn_local(report_bug(harness.context, None));

        submit_description(&harness.editor_container).await;
        select_option(&harness.editor_container, 1).await;
        select_option(&harness.editor_container, 1).await;

        let result = handle.await.expect("report bug task");
        assert!(is_shared_editor(&harness.editor_container, &harness.editor));
        result
    })
    .await;

    let error: SessionManagerError = result.expect_err("malformed gateway rejects reportBug");
    assert_eq!(error.to_string(), "Invalid URL");
    assert!(errors.borrow().is_empty(), "errors: {:?}", errors.borrow());
    assert!(statuses.borrow().is_empty());
}
