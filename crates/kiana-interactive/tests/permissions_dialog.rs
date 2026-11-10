//! `PermissionsDialog` 与 `ApprovalModeIndicator` 的渲染/按键/回调测试。
//!
//! 组件文件通过 `#[path]` 直接编进测试 crate：`components/mod.rs` 由 Integrate agent
//! 统一声明，本任务不修改共享文件，因此这里构造一个本地 `components` 命名空间来解析
//! 组件内部的 `crate::components::approval_mode_indicator` 引用。Integrate 声明模块后，
//! 该测试的引入方式可以原样保留（两边的模块路径同名同形）。
//!
//! 锁定行为：
//! - 四个标签页、`+ Add rule…` 首行、规则来源列与空标签页提示；
//! - Tab/Shift+Tab/←/→ 的循环顺序；
//! - `/` 过滤的进入/提交/清除，以及过滤中 `m`/`d` 作为文本；
//! - 新增规则/目录的解析、作用域选择、失败保留视图；
//! - 删除规则/目录的确认、managed/CLI 拒删、cwd 拒删；
//! - 模式切换（含作用域选择与 bypass 禁用）；
//! - `ApprovalModeIndicator` 的空态、文本、颜色与加粗。

#[path = "../src/components/approval_mode_indicator.rs"]
pub mod approval_mode_indicator;
#[path = "../src/components/permissions_dialog.rs"]
pub mod permissions_dialog;

/// 组件内部的 `crate::components::…` 引用在测试 crate 里的等价命名空间。
mod components {
    pub use super::approval_mode_indicator;
    pub use super::permissions_dialog;
}

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard};

use components::approval_mode_indicator::{
    ApprovalModeIndicator, PermissionModeVisual, permission_mode_visual,
};
use components::permissions_dialog::{
    ModePersistenceScope, PanelNotice, PermissionRuleEntry, PermissionRuleScope,
    PermissionRuleScopeChoice, PermissionSettingsLayer, PermissionSnapshot,
    PermissionSnapshotInput, PermissionsCallbacks, PermissionsDialog, PermissionsDialogOptions,
    PermissionsView, RuleTab, collect_settings_layers, display_rule_text,
    parse_settings_rule_value,
};
use kiana_config::settings::{SettingsManager, SettingsManagerCreateOptions};
use kiana_permissions::{
    PermissionEngine, PermissionFlags, PermissionMode, PermissionRule, RuleDecision, parse_cli_rule,
};
use kiana_theme::ThemeColor;
use kiana_tui::text::visible_width;
use kiana_tui::tui::{Component, Focusable};
use serde_json::json;

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

/// 主题是进程级全局状态；测试并行运行，用一把锁串行化初始化与渲染。
fn setup() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    let guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    kiana_theme::init_theme(Some("dark"), false);
    guard
}

/// 去掉 CSI/OSC/APC 转义序列（测试只需比较可见文本）。
fn strip_ansi(text: &str) -> String {
    let mut output = String::new();
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\x1b' {
            output.push(character);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') | Some('_') => {
                chars.next();
                for next in chars.by_ref() {
                    if next == '\x07' {
                        break;
                    }
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    output
}

fn render_plain(dialog: &mut PermissionsDialog, width: usize) -> Vec<String> {
    dialog
        .render(width)
        .iter()
        .map(|line| strip_ansi(line))
        .collect()
}

fn joined(dialog: &mut PermissionsDialog, width: usize) -> String {
    render_plain(dialog, width).join("\n")
}

fn entry(
    rule: PermissionRule,
    scopes: &[PermissionRuleScope],
    removable: bool,
) -> PermissionRuleEntry {
    PermissionRuleEntry {
        raw: rule.to_settings_value(),
        decision: rule.decision(),
        rule,
        scopes: scopes.to_vec(),
        removable,
    }
}

fn base_snapshot() -> PermissionSnapshot {
    PermissionSnapshot {
        mode: Some(PermissionMode::Default),
        bypass_disabled: false,
        rules: vec![
            entry(
                PermissionRule::tool("edit", RuleDecision::Allow),
                &[PermissionRuleScope::Project],
                true,
            ),
            entry(
                PermissionRule::tool("read", RuleDecision::Allow),
                &[PermissionRuleScope::User],
                true,
            ),
            entry(
                PermissionRule::command(vec!["cargo".into(), "test".into()], RuleDecision::Ask),
                &[PermissionRuleScope::Local, PermissionRuleScope::User],
                true,
            ),
            entry(
                PermissionRule::path("/etc", RuleDecision::Deny),
                &[PermissionRuleScope::ManagedOrCli],
                false,
            ),
        ],
        workspace_roots: vec![PathBuf::from("/repo"), PathBuf::from("/repo/extra")],
        workspace_scopes: vec![Vec::new(), vec![PermissionRuleScope::Project]],
        session_allow_count: 0,
        errors: Vec::new(),
    }
}

fn dialog(snapshot: PermissionSnapshot, callbacks: PermissionsCallbacks) -> PermissionsDialog {
    PermissionsDialog::new(PermissionsDialogOptions {
        snapshot,
        cwd: PathBuf::from("/repo"),
        project_trusted: true,
        callbacks,
    })
}

fn type_text(dialog: &mut PermissionsDialog, text: &str) {
    dialog.handle_input(text);
}

fn enter(dialog: &mut PermissionsDialog) {
    dialog.handle_input("\r");
}

fn escape(dialog: &mut PermissionsDialog) {
    dialog.handle_input("\x1b");
}

fn down(dialog: &mut PermissionsDialog) {
    dialog.handle_input("\x1b[B");
}

fn up(dialog: &mut PermissionsDialog) {
    dialog.handle_input("\x1b[A");
}

fn open_add_rule(dialog: &mut PermissionsDialog) {
    enter(dialog);
    assert!(
        matches!(dialog.view(), PermissionsView::AddRuleInput { .. }),
        "Enter on the add row must open the rule input, got {:?}",
        dialog.view()
    );
}

fn open_workspace(dialog: &mut PermissionsDialog) {
    for _ in 0..3 {
        dialog.handle_input("\t");
    }
    assert_eq!(dialog.tab(), RuleTab::Workspace);
    assert!(matches!(dialog.view(), PermissionsView::WorkspaceList));
}

fn assert_contains(haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "expected {needle:?} in:\n{haystack}"
    );
}

fn assert_not_contains(haystack: &str, needle: &str) {
    assert!(
        !haystack.contains(needle),
        "did not expect {needle:?} in:\n{haystack}"
    );
}

// ---------------------------------------------------------------------------
// 渲染
// ---------------------------------------------------------------------------

#[test]
fn renders_header_tabs_rows_and_scopes() {
    let _guard = setup();
    let mut dialog = dialog(base_snapshot(), PermissionsCallbacks::default());
    let text = joined(&mut dialog, 100);

    assert_contains(&text, "Permissions");
    assert_contains(&text, "Mode: default");
    assert_contains(&text, "[Allow]  Ask  Deny  Workspace");
    assert_contains(&text, "+ Add rule…");
    assert_contains(&text, "tool:edit");
    assert_contains(&text, "project");
    assert_contains(&text, "Enter select · Tab switch · Esc close");
}

#[test]
fn ask_tab_shows_only_ask_rules_with_all_scopes() {
    let _guard = setup();
    let mut dialog = dialog(base_snapshot(), PermissionsCallbacks::default());
    dialog.handle_input("\t");
    assert_eq!(dialog.tab(), RuleTab::Ask);
    let text = joined(&mut dialog, 100);
    assert_contains(&text, "[Ask]");
    assert_contains(&text, "command:cargo test");
    assert_contains(&text, "local, user");
    assert_not_contains(&text, "tool:edit");
}

#[test]
fn empty_tab_has_message() {
    let _guard = setup();
    let mut snapshot = base_snapshot();
    snapshot
        .rules
        .retain(|entry| entry.decision != RuleDecision::Deny);
    let mut dialog = dialog(snapshot, PermissionsCallbacks::default());
    dialog.handle_input("\t");
    dialog.handle_input("\t");
    assert_eq!(dialog.tab(), RuleTab::Deny);
    assert_contains(&joined(&mut dialog, 100), "No deny rules.");
}

#[test]
fn tab_cycles_forwards_and_backwards() {
    let _guard = setup();
    let mut dialog = dialog(base_snapshot(), PermissionsCallbacks::default());
    dialog.handle_input("\t");
    assert_eq!(dialog.tab(), RuleTab::Ask);
    dialog.handle_input("\t");
    assert_eq!(dialog.tab(), RuleTab::Deny);
    dialog.handle_input("\t");
    assert_eq!(dialog.tab(), RuleTab::Workspace);
    assert!(matches!(dialog.view(), PermissionsView::WorkspaceList));
    dialog.handle_input("\t");
    assert_eq!(dialog.tab(), RuleTab::Allow);

    dialog.handle_input("\x1b[Z");
    assert_eq!(dialog.tab(), RuleTab::Workspace);
    dialog.handle_input("\x1b[C");
    assert_eq!(dialog.tab(), RuleTab::Allow);
    dialog.handle_input("\x1b[D");
    assert_eq!(dialog.tab(), RuleTab::Workspace);
}

#[test]
fn errors_are_rendered_as_warnings() {
    let _guard = setup();
    let mut snapshot = base_snapshot();
    snapshot.errors = vec!["Invalid JSON in settings.local.json".to_string()];
    let mut dialog = dialog(snapshot, PermissionsCallbacks::default());
    assert_contains(
        &joined(&mut dialog, 100),
        "⚠ Invalid JSON in settings.local.json",
    );
}

#[test]
fn mode_off_is_rendered_as_off() {
    let _guard = setup();
    let mut snapshot = base_snapshot();
    snapshot.mode = None;
    let mut dialog = dialog(snapshot, PermissionsCallbacks::default());
    assert_contains(&joined(&mut dialog, 100), "Mode: off");
}

#[test]
fn render_keeps_lines_within_width() {
    let _guard = setup();
    for width in [20usize, 40, 80, 120] {
        let mut dialog = dialog(base_snapshot(), PermissionsCallbacks::default());
        for line in render_plain(&mut dialog, width) {
            assert!(
                visible_width(&line) <= width,
                "line wider than {width}: {line:?}"
            );
        }
    }
    // 退化宽度不能 panic。
    let mut dialog = dialog(base_snapshot(), PermissionsCallbacks::default());
    let _ = dialog.render(0);
    let _ = dialog.render(1);
}

// ---------------------------------------------------------------------------
// 新增规则
// ---------------------------------------------------------------------------

#[test]
fn add_rule_flow_calls_callback_with_scope() {
    let _guard = setup();
    let added: Rc<RefCell<Vec<(PermissionRule, PermissionRuleScopeChoice)>>> =
        Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&added);
    let callbacks = PermissionsCallbacks {
        on_add_rule: Box::new(move |rule, scope| {
            sink.borrow_mut().push((rule.clone(), scope));
            Ok(())
        }),
        ..Default::default()
    };
    let mut dialog = dialog(base_snapshot(), callbacks);

    open_add_rule(&mut dialog);
    type_text(&mut dialog, "command:cargo build");
    enter(&mut dialog);
    assert!(
        matches!(
            dialog.view(),
            PermissionsView::AddRuleScope { selected: 0, .. }
        ),
        "trusted project defaults to Project scope, got {:?}",
        dialog.view()
    );
    enter(&mut dialog);

    let recorded = added.borrow();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].0,
        PermissionRule::command(vec!["cargo".into(), "build".into()], RuleDecision::Allow)
    );
    assert_eq!(recorded[0].1, PermissionRuleScopeChoice::Project);
    assert!(matches!(
        dialog.view(),
        PermissionsView::RuleList {
            tab: RuleTab::Allow
        }
    ));
    assert_eq!(
        dialog.notice().map(PanelNotice::text),
        Some("Added command:cargo build")
    );
}

#[test]
fn malformed_rule_keeps_input_and_shows_error() {
    let _guard = setup();
    let mut dialog = dialog(base_snapshot(), PermissionsCallbacks::default());
    open_add_rule(&mut dialog);
    type_text(&mut dialog, "tool:");
    enter(&mut dialog);
    match dialog.view() {
        PermissionsView::AddRuleInput { text, error } => {
            assert_eq!(text, "tool:");
            assert!(
                error
                    .as_deref()
                    .is_some_and(|text| text.starts_with("Malformed rule."))
            );
        }
        other => panic!("expected AddRuleInput, got {other:?}"),
    }
}

#[test]
fn add_rule_is_rejected_when_permission_layer_is_off() {
    let _guard = setup();
    let called = Rc::new(RefCell::new(false));
    let flag = Rc::clone(&called);
    let callbacks = PermissionsCallbacks {
        on_add_rule: Box::new(move |_, _| {
            *flag.borrow_mut() = true;
            Ok(())
        }),
        ..Default::default()
    };
    let mut snapshot = base_snapshot();
    snapshot.mode = None;
    let mut dialog = dialog(snapshot, callbacks);
    open_add_rule(&mut dialog);
    type_text(&mut dialog, "tool:read");
    enter(&mut dialog);
    enter(&mut dialog);
    match dialog.view() {
        PermissionsView::AddRuleScope { error, .. } => {
            assert_eq!(
                error.as_deref(),
                Some("Permission checks are disabled. Change the mode first.")
            );
        }
        other => panic!("expected AddRuleScope, got {other:?}"),
    }
    assert!(!*called.borrow());
}

#[test]
fn untrusted_project_defaults_to_user_scope() {
    let _guard = setup();
    let added: Rc<RefCell<Vec<PermissionRuleScopeChoice>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&added);
    let callbacks = PermissionsCallbacks {
        on_add_rule: Box::new(move |_, scope| {
            sink.borrow_mut().push(scope);
            Ok(())
        }),
        ..Default::default()
    };
    let mut dialog = PermissionsDialog::new(PermissionsDialogOptions {
        snapshot: base_snapshot(),
        cwd: PathBuf::from("/repo"),
        project_trusted: false,
        callbacks,
    });
    open_add_rule(&mut dialog);
    type_text(&mut dialog, "tool:read");
    enter(&mut dialog);
    assert!(matches!(
        dialog.view(),
        PermissionsView::AddRuleScope { selected: 2, .. }
    ));
    enter(&mut dialog);
    assert_eq!(*added.borrow(), vec![PermissionRuleScopeChoice::User]);
}

#[test]
fn untrusted_project_scope_is_rejected() {
    let _guard = setup();
    let mut dialog = PermissionsDialog::new(PermissionsDialogOptions {
        snapshot: base_snapshot(),
        cwd: PathBuf::from("/repo"),
        project_trusted: false,
        callbacks: PermissionsCallbacks::default(),
    });
    open_add_rule(&mut dialog);
    type_text(&mut dialog, "tool:read");
    enter(&mut dialog);
    // 默认选中 User（2）；上移两次到 Project（0）。
    up(&mut dialog);
    up(&mut dialog);
    enter(&mut dialog);
    match dialog.view() {
        PermissionsView::AddRuleScope { error, .. } => {
            assert_eq!(
                error.as_deref(),
                Some("Project is not trusted; cannot write.")
            );
        }
        other => panic!("expected AddRuleScope, got {other:?}"),
    }
}

#[test]
fn add_rule_scope_escape_returns_to_input_with_text() {
    let _guard = setup();
    let mut dialog = dialog(base_snapshot(), PermissionsCallbacks::default());
    open_add_rule(&mut dialog);
    type_text(&mut dialog, "tool:read");
    enter(&mut dialog);
    escape(&mut dialog);
    match dialog.view() {
        PermissionsView::AddRuleInput { text, .. } => assert_eq!(text, "tool:read"),
        other => panic!("expected AddRuleInput, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 删除规则
// ---------------------------------------------------------------------------

#[test]
fn delete_rule_confirm_defaults_to_yes_and_calls_callback() {
    let _guard = setup();
    let deleted: Rc<RefCell<Vec<PermissionRuleEntry>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&deleted);
    let callbacks = PermissionsCallbacks {
        on_delete_rule: Box::new(move |entry| {
            sink.borrow_mut().push(entry.clone());
            Ok(())
        }),
        ..Default::default()
    };
    let mut dialog = dialog(base_snapshot(), callbacks);
    down(&mut dialog);
    assert_eq!(dialog.selected_rule(), 1);
    dialog.handle_input("d");
    assert!(matches!(
        dialog.view(),
        PermissionsView::DeleteRuleConfirm { selected: 1, .. }
    ));
    let text = joined(&mut dialog, 100);
    assert_contains(&text, "Delete permission rule?");
    assert_contains(&text, "scope:");
    assert_contains(&text, "[ ] No   [x] Yes");

    enter(&mut dialog);
    let recorded = deleted.borrow();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].rule,
        PermissionRule::tool("edit", RuleDecision::Allow)
    );
    assert!(matches!(
        dialog.view(),
        PermissionsView::RuleList {
            tab: RuleTab::Allow
        }
    ));
    assert_eq!(
        dialog.notice().map(PanelNotice::text),
        Some("Removed tool:edit")
    );
}

#[test]
fn delete_rule_escape_cancels() {
    let _guard = setup();
    let called = Rc::new(RefCell::new(false));
    let flag = Rc::clone(&called);
    let callbacks = PermissionsCallbacks {
        on_delete_rule: Box::new(move |_| {
            *flag.borrow_mut() = true;
            Ok(())
        }),
        ..Default::default()
    };
    let mut dialog = dialog(base_snapshot(), callbacks);
    down(&mut dialog);
    dialog.handle_input("d");
    escape(&mut dialog);
    assert!(matches!(dialog.view(), PermissionsView::RuleList { .. }));
    assert!(!*called.borrow());
}

#[test]
fn managed_rule_cannot_be_deleted() {
    let _guard = setup();
    let called = Rc::new(RefCell::new(false));
    let flag = Rc::clone(&called);
    let callbacks = PermissionsCallbacks {
        on_delete_rule: Box::new(move |_| {
            *flag.borrow_mut() = true;
            Ok(())
        }),
        ..Default::default()
    };
    let mut dialog = dialog(base_snapshot(), callbacks);
    dialog.handle_input("\t");
    dialog.handle_input("\t");
    assert_eq!(dialog.tab(), RuleTab::Deny);
    down(&mut dialog);
    dialog.handle_input("d");
    assert_contains(&joined(&mut dialog, 120), "cannot be removed here");
    enter(&mut dialog);
    assert!(!*called.borrow());
    let notice = dialog.notice().expect("notice");
    assert!(notice.is_error());
    assert_contains(notice.text(), "cannot be removed here");
}

#[test]
fn delete_rule_write_error_keeps_confirm_view() {
    let _guard = setup();
    let callbacks = PermissionsCallbacks {
        on_delete_rule: Box::new(|_| Err("disk full".to_string())),
        ..Default::default()
    };
    let mut dialog = dialog(base_snapshot(), callbacks);
    down(&mut dialog);
    dialog.handle_input("d");
    enter(&mut dialog);
    match dialog.view() {
        PermissionsView::DeleteRuleConfirm { error, .. } => {
            assert_eq!(error.as_deref(), Some("disk full"));
        }
        other => panic!("expected DeleteRuleConfirm, got {other:?}"),
    }
    assert_contains(&joined(&mut dialog, 100), "disk full");
}

// ---------------------------------------------------------------------------
// 工作区目录
// ---------------------------------------------------------------------------

#[test]
fn workspace_tab_lists_cwd_and_extra_directories() {
    let _guard = setup();
    let mut dialog = dialog(base_snapshot(), PermissionsCallbacks::default());
    open_workspace(&mut dialog);
    let text = joined(&mut dialog, 100);
    assert_contains(&text, "[Workspace]");
    assert_contains(&text, "+ Add directory…");
    assert_contains(&text, "/repo");
    assert_contains(&text, "working directory (always included)");
    assert_contains(&text, "/repo/extra");
    assert_contains(&text, "project");
}

#[test]
fn working_directory_cannot_be_removed() {
    let _guard = setup();
    let mut dialog = dialog(base_snapshot(), PermissionsCallbacks::default());
    open_workspace(&mut dialog);
    down(&mut dialog);
    assert_eq!(dialog.selected_dir(), 1);
    dialog.handle_input("d");
    assert!(matches!(dialog.view(), PermissionsView::WorkspaceList));
    let notice = dialog.notice().expect("notice");
    assert!(notice.is_error());
    assert_contains(notice.text(), "working directory cannot be removed");
}

#[test]
fn add_directory_flow_calls_callback() {
    let _guard = setup();
    let root = tempfile::tempdir().expect("temp");
    // 目录位于 cwd 之外（cwd 的子目录会被“inside an already-included directory”拒绝）。
    let extra = tempfile::tempdir().expect("extra");
    let extra_path = extra.path().to_path_buf();

    let added: Rc<RefCell<Vec<(PathBuf, PermissionRuleScopeChoice)>>> =
        Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&added);
    let callbacks = PermissionsCallbacks {
        on_add_directory: Box::new(move |path, scope| {
            sink.borrow_mut().push((path.to_path_buf(), scope));
            Ok(())
        }),
        ..Default::default()
    };
    let mut snapshot = base_snapshot();
    snapshot.workspace_roots = vec![root.path().to_path_buf()];
    snapshot.workspace_scopes = vec![Vec::new()];
    let mut dialog = PermissionsDialog::new(PermissionsDialogOptions {
        snapshot,
        cwd: root.path().to_path_buf(),
        project_trusted: true,
        callbacks,
    });

    open_workspace(&mut dialog);
    enter(&mut dialog);
    assert!(matches!(
        dialog.view(),
        PermissionsView::AddDirectoryInput { .. }
    ));
    type_text(&mut dialog, &extra_path.to_string_lossy());
    enter(&mut dialog);
    assert!(matches!(
        dialog.view(),
        PermissionsView::AddDirectoryScope { selected: 0, .. }
    ));
    enter(&mut dialog);

    let recorded = added.borrow();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0, extra_path);
    assert_eq!(recorded[0].1, PermissionRuleScopeChoice::Project);
    assert!(matches!(dialog.view(), PermissionsView::WorkspaceList));
    assert!(
        dialog
            .notice()
            .is_some_and(|notice| notice.text().contains("Added directory"))
    );
}

#[test]
fn add_directory_rejects_missing_and_non_directory_paths() {
    let _guard = setup();
    let root = tempfile::tempdir().expect("temp");
    let file = root.path().join("file.txt");
    std::fs::write(&file, "x").expect("write");

    // 不存在的目录。
    let mut snapshot = base_snapshot();
    snapshot.workspace_roots = vec![root.path().to_path_buf()];
    snapshot.workspace_scopes = vec![Vec::new()];
    let mut dialog = PermissionsDialog::new(PermissionsDialogOptions {
        snapshot,
        cwd: root.path().to_path_buf(),
        project_trusted: true,
        callbacks: PermissionsCallbacks::default(),
    });
    open_workspace(&mut dialog);
    enter(&mut dialog);
    type_text(&mut dialog, &root.path().join("missing").to_string_lossy());
    enter(&mut dialog);
    match dialog.view() {
        PermissionsView::AddDirectoryInput { error, .. } => {
            assert_eq!(error.as_deref(), Some("Directory does not exist."));
        }
        other => panic!("expected AddDirectoryInput, got {other:?}"),
    }

    // 存在但不是目录。
    let mut snapshot = base_snapshot();
    snapshot.workspace_roots = vec![root.path().to_path_buf()];
    snapshot.workspace_scopes = vec![Vec::new()];
    let mut dialog = PermissionsDialog::new(PermissionsDialogOptions {
        snapshot,
        cwd: root.path().to_path_buf(),
        project_trusted: true,
        callbacks: PermissionsCallbacks::default(),
    });
    open_workspace(&mut dialog);
    enter(&mut dialog);
    type_text(&mut dialog, &file.to_string_lossy());
    enter(&mut dialog);
    match dialog.view() {
        PermissionsView::AddDirectoryInput { error, .. } => {
            assert_eq!(error.as_deref(), Some("Path is not a directory."));
        }
        other => panic!("expected AddDirectoryInput, got {other:?}"),
    }
}

#[test]
fn add_directory_rejects_duplicate_and_subdirectory() {
    let _guard = setup();
    let root = tempfile::tempdir().expect("temp");
    let extra = tempfile::tempdir().expect("extra");
    let nested = root.path().join("nested");
    std::fs::create_dir_all(&nested).expect("mkdir");

    // 重复目录。
    let mut snapshot = base_snapshot();
    snapshot.workspace_roots = vec![root.path().to_path_buf(), extra.path().to_path_buf()];
    snapshot.workspace_scopes = vec![Vec::new(), vec![PermissionRuleScope::Project]];
    let mut dialog = PermissionsDialog::new(PermissionsDialogOptions {
        snapshot,
        cwd: root.path().to_path_buf(),
        project_trusted: true,
        callbacks: PermissionsCallbacks::default(),
    });
    open_workspace(&mut dialog);
    enter(&mut dialog);
    type_text(&mut dialog, &extra.path().to_string_lossy());
    enter(&mut dialog);
    match dialog.view() {
        PermissionsView::AddDirectoryInput { error, .. } => {
            assert_eq!(
                error.as_deref(),
                Some("This directory is already in the workspace.")
            );
        }
        other => panic!("expected AddDirectoryInput, got {other:?}"),
    }

    // 已有根的子目录。
    let mut snapshot = base_snapshot();
    snapshot.workspace_roots = vec![root.path().to_path_buf()];
    snapshot.workspace_scopes = vec![Vec::new()];
    let mut dialog = PermissionsDialog::new(PermissionsDialogOptions {
        snapshot,
        cwd: root.path().to_path_buf(),
        project_trusted: true,
        callbacks: PermissionsCallbacks::default(),
    });
    open_workspace(&mut dialog);
    enter(&mut dialog);
    type_text(&mut dialog, &nested.to_string_lossy());
    enter(&mut dialog);
    match dialog.view() {
        PermissionsView::AddDirectoryInput { error, .. } => {
            assert_eq!(
                error.as_deref(),
                Some("This directory is inside an already-included directory.")
            );
        }
        other => panic!("expected AddDirectoryInput, got {other:?}"),
    }
}

#[test]
fn remove_directory_confirm_calls_callback() {
    let _guard = setup();
    let root = tempfile::tempdir().expect("temp");
    let extra = root.path().join("extra");
    std::fs::create_dir_all(&extra).expect("mkdir");

    let removed: Rc<RefCell<Vec<PathBuf>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&removed);
    let callbacks = PermissionsCallbacks {
        on_remove_directory: Box::new(move |path| {
            sink.borrow_mut().push(path.to_path_buf());
            Ok(())
        }),
        ..Default::default()
    };
    let mut snapshot = base_snapshot();
    snapshot.workspace_roots = vec![root.path().to_path_buf(), extra.clone()];
    snapshot.workspace_scopes = vec![Vec::new(), vec![PermissionRuleScope::Project]];
    let mut dialog = PermissionsDialog::new(PermissionsDialogOptions {
        snapshot,
        cwd: root.path().to_path_buf(),
        project_trusted: true,
        callbacks,
    });

    open_workspace(&mut dialog);
    down(&mut dialog);
    down(&mut dialog);
    assert_eq!(dialog.selected_dir(), 2);
    dialog.handle_input("d");
    assert!(matches!(
        dialog.view(),
        PermissionsView::RemoveDirectoryConfirm { selected: 1, .. }
    ));
    enter(&mut dialog);
    assert_eq!(*removed.borrow(), vec![extra]);
    assert!(matches!(dialog.view(), PermissionsView::WorkspaceList));
}

#[test]
fn managed_directory_cannot_be_removed() {
    let _guard = setup();
    let called = Rc::new(RefCell::new(false));
    let flag = Rc::clone(&called);
    let callbacks = PermissionsCallbacks {
        on_remove_directory: Box::new(move |_| {
            *flag.borrow_mut() = true;
            Ok(())
        }),
        ..Default::default()
    };
    let mut snapshot = base_snapshot();
    snapshot.workspace_scopes = vec![Vec::new(), vec![PermissionRuleScope::ManagedOrCli]];
    let mut dialog = dialog(snapshot, callbacks);
    open_workspace(&mut dialog);
    down(&mut dialog);
    down(&mut dialog);
    dialog.handle_input("d");
    assert_contains(&joined(&mut dialog, 120), "cannot be removed here");
    enter(&mut dialog);
    assert!(!*called.borrow());
    assert!(dialog.notice().is_some_and(PanelNotice::is_error));
}

// ---------------------------------------------------------------------------
// 过滤
// ---------------------------------------------------------------------------

#[test]
fn filter_uses_substring_and_m_is_text_while_active() {
    let _guard = setup();
    let closed = Rc::new(RefCell::new(0usize));
    let flag = Rc::clone(&closed);
    let callbacks = PermissionsCallbacks {
        on_close: Box::new(move || {
            *flag.borrow_mut() += 1;
        }),
        ..Default::default()
    };
    let mut snapshot = base_snapshot();
    snapshot.rules.insert(
        1,
        entry(
            PermissionRule::command(vec!["git".into(), "status".into()], RuleDecision::Allow),
            &[PermissionRuleScope::User],
            true,
        ),
    );
    let mut dialog = dialog(snapshot, callbacks);

    dialog.handle_input("/");
    assert!(dialog.is_filter_active());
    type_text(&mut dialog, "git");
    assert_eq!(dialog.rule_query(), "git");
    let text = joined(&mut dialog, 100);
    assert_contains(&text, "command:git status");
    assert_not_contains(&text, "tool:edit");
    assert_contains(&text, "filter: git");

    // 过滤模式下 `m` 是文本，不是命令。
    type_text(&mut dialog, "m");
    assert_eq!(dialog.rule_query(), "gitm");
    assert!(matches!(dialog.view(), PermissionsView::RuleList { .. }));

    // 第一次 Esc 清空并退出过滤；第二次 Esc 关闭面板。
    escape(&mut dialog);
    assert!(!dialog.is_filter_active());
    assert_eq!(dialog.rule_query(), "");
    escape(&mut dialog);
    assert_eq!(*closed.borrow(), 1);
}

#[test]
fn filter_enter_commits_but_keeps_query() {
    let _guard = setup();
    let mut dialog = dialog(base_snapshot(), PermissionsCallbacks::default());
    dialog.handle_input("/");
    type_text(&mut dialog, "edit");
    enter(&mut dialog);
    assert!(!dialog.is_filter_active());
    assert_eq!(dialog.rule_query(), "edit");
    let text = joined(&mut dialog, 100);
    assert_contains(&text, "filter: edit");
    assert_contains(&text, "tool:edit");
}

// ---------------------------------------------------------------------------
// 模式切换
// ---------------------------------------------------------------------------

#[test]
fn mode_select_applies_mode_with_session_scope() {
    let _guard = setup();
    let selected: Rc<RefCell<Vec<(PermissionMode, ModePersistenceScope)>>> =
        Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&selected);
    let callbacks = PermissionsCallbacks {
        on_set_mode: Box::new(move |mode, scope| {
            sink.borrow_mut().push((mode, scope));
            Ok(())
        }),
        ..Default::default()
    };
    let mut dialog = dialog(base_snapshot(), callbacks);

    dialog.handle_input("m");
    assert!(matches!(
        dialog.view(),
        PermissionsView::ModeSelect { selected: 1, .. }
    ));
    let text = joined(&mut dialog, 120);
    assert_contains(&text, "Select permission mode");
    assert_contains(&text, "allow edits inside the workspace");

    down(&mut dialog);
    assert!(matches!(
        dialog.view(),
        PermissionsView::ModeSelect { selected: 2, .. }
    ));
    enter(&mut dialog);
    assert!(matches!(
        dialog.view(),
        PermissionsView::ModeScopeSelect { selected: 0, .. }
    ));
    let text = joined(&mut dialog, 120);
    assert_contains(&text, "Save permission mode");
    assert_contains(&text, "accept-edits");
    assert_contains(&text, "Session only");

    enter(&mut dialog);
    assert_eq!(
        *selected.borrow(),
        vec![(PermissionMode::AcceptEdits, ModePersistenceScope::Session)]
    );
    assert_eq!(dialog.snapshot().mode, Some(PermissionMode::AcceptEdits));
    assert_contains(&joined(&mut dialog, 100), "Mode: accept-edits");
}

#[test]
fn bypass_is_rejected_when_disabled_by_policy() {
    let _guard = setup();
    let called = Rc::new(RefCell::new(false));
    let flag = Rc::clone(&called);
    let callbacks = PermissionsCallbacks {
        on_set_mode: Box::new(move |_, _| {
            *flag.borrow_mut() = true;
            Ok(())
        }),
        ..Default::default()
    };
    let mut snapshot = base_snapshot();
    snapshot.bypass_disabled = true;
    let mut dialog = dialog(snapshot, callbacks);
    dialog.handle_input("m");
    assert_contains(&joined(&mut dialog, 120), "Disabled by policy.");
    down(&mut dialog);
    down(&mut dialog);
    assert!(matches!(
        dialog.view(),
        PermissionsView::ModeSelect { selected: 3, .. }
    ));
    enter(&mut dialog);
    assert!(matches!(dialog.view(), PermissionsView::ModeSelect { .. }));
    assert!(!*called.borrow());
    assert_eq!(
        dialog.notice().map(PanelNotice::text),
        Some("Bypass mode is disabled by policy.")
    );
}

#[test]
fn mode_persistence_scope_requires_trust() {
    let _guard = setup();
    let mut dialog = PermissionsDialog::new(PermissionsDialogOptions {
        snapshot: base_snapshot(),
        cwd: PathBuf::from("/repo"),
        project_trusted: false,
        callbacks: PermissionsCallbacks::default(),
    });
    dialog.handle_input("m");
    enter(&mut dialog);
    assert!(matches!(
        dialog.view(),
        PermissionsView::ModeScopeSelect { selected: 0, .. }
    ));
    down(&mut dialog);
    assert!(matches!(
        dialog.view(),
        PermissionsView::ModeScopeSelect { selected: 1, .. }
    ));
    enter(&mut dialog);
    assert_eq!(
        dialog.notice().map(PanelNotice::text),
        Some("Project is not trusted; cannot write.")
    );
    assert!(matches!(
        dialog.view(),
        PermissionsView::ModeScopeSelect { .. }
    ));
}

#[test]
fn mode_select_escape_returns_to_list() {
    let _guard = setup();
    let mut dialog = dialog(base_snapshot(), PermissionsCallbacks::default());
    dialog.handle_input("m");
    escape(&mut dialog);
    assert!(matches!(dialog.view(), PermissionsView::RuleList { .. }));
}

#[test]
fn mode_select_from_workspace_returns_to_workspace_list() {
    let _guard = setup();
    let mut dialog = dialog(base_snapshot(), PermissionsCallbacks::default());
    open_workspace(&mut dialog);
    dialog.handle_input("m");
    escape(&mut dialog);
    assert!(matches!(dialog.view(), PermissionsView::WorkspaceList));
    // 应用模式后也应回到工作区列表，而不是一个空的 RuleList { tab: Workspace }。
    dialog.handle_input("m");
    enter(&mut dialog);
    enter(&mut dialog);
    assert!(matches!(dialog.view(), PermissionsView::WorkspaceList));
    assert_eq!(dialog.tab(), RuleTab::Workspace);
}

// ---------------------------------------------------------------------------
// 回调与焦点
// ---------------------------------------------------------------------------

#[test]
fn refresh_key_calls_refresh_callback() {
    let _guard = setup();
    let refreshed = Rc::new(RefCell::new(0usize));
    let flag = Rc::clone(&refreshed);
    let callbacks = PermissionsCallbacks {
        on_refresh: Box::new(move || {
            *flag.borrow_mut() += 1;
        }),
        ..Default::default()
    };
    let mut dialog = dialog(base_snapshot(), callbacks);
    dialog.handle_input("r");
    assert_eq!(*refreshed.borrow(), 1);
    // `r` 在过滤模式下是文本。
    dialog.handle_input("/");
    type_text(&mut dialog, "r");
    assert_eq!(dialog.rule_query(), "r");
    assert_eq!(*refreshed.borrow(), 1);
}

#[test]
fn escape_closes_from_rule_list() {
    let _guard = setup();
    let closed = Rc::new(RefCell::new(false));
    let flag = Rc::clone(&closed);
    let callbacks = PermissionsCallbacks {
        on_close: Box::new(move || {
            *flag.borrow_mut() = true;
        }),
        ..Default::default()
    };
    let mut dialog = dialog(base_snapshot(), callbacks);
    escape(&mut dialog);
    assert!(*closed.borrow());
}

#[test]
fn dialog_is_focusable() {
    let _guard = setup();
    let mut dialog = dialog(base_snapshot(), PermissionsCallbacks::default());
    assert!(dialog.handles_input());
    assert!(!dialog.focused());
    dialog.set_focused(true);
    assert!(dialog.focused());
    assert!(dialog.as_focusable().is_some());
}

#[test]
fn set_snapshot_clamps_selection() {
    let _guard = setup();
    let mut dialog = dialog(base_snapshot(), PermissionsCallbacks::default());
    dialog.handle_input("\t");
    // Ask 标签页只有 1 条规则；把选中项压到最大值后换成空快照。
    down(&mut dialog);
    assert_eq!(dialog.selected_rule(), 1);
    let mut empty = base_snapshot();
    empty.rules.clear();
    dialog.set_snapshot(empty);
    assert_eq!(dialog.selected_rule(), 0);
}

// ---------------------------------------------------------------------------
// 快照组装
// ---------------------------------------------------------------------------

#[test]
fn snapshot_build_attributes_rules_and_directories_to_layers() {
    let _guard = setup();
    let cwd = PathBuf::from("/repo");
    let rule_read = json!({"tool": "read", "decision": "allow"});
    let rule_path = json!({"path": "/etc", "decision": "deny"});
    let layers = vec![
        PermissionSettingsLayer::new(
            PermissionRuleScope::User,
            Some(json!({
                "rules": [rule_read.clone()],
                "allow": ["command:git status"],
                "addDirs": ["/repo/extra"],
            })),
        ),
        PermissionSettingsLayer::new(
            PermissionRuleScope::Project,
            Some(json!({"rules": [rule_path.clone()]})),
        ),
    ];
    let policy_rules = vec![
        parse_settings_rule_value(&rule_read, &cwd).expect("read rule"),
        parse_cli_rule("command:git status", RuleDecision::Allow).expect("shorthand rule"),
        parse_settings_rule_value(&rule_path, &cwd).expect("path rule"),
    ];
    let snapshot = PermissionSnapshot::build(PermissionSnapshotInput {
        cwd: cwd.clone(),
        mode: Some(PermissionMode::Default),
        bypass_disabled: false,
        policy_rules,
        workspace_roots: vec![cwd.clone(), PathBuf::from("/repo/extra")],
        session_allow_count: 3,
        layers,
        errors: Vec::new(),
    });

    assert_eq!(snapshot.rules.len(), 3);
    assert_eq!(snapshot.rules[0].scopes, vec![PermissionRuleScope::User]);
    assert!(snapshot.rules[0].removable);
    assert_eq!(snapshot.rules[1].scopes, vec![PermissionRuleScope::User]);
    assert_eq!(snapshot.rules[2].scopes, vec![PermissionRuleScope::Project]);
    assert_eq!(snapshot.session_allow_count, 3);
    assert_eq!(
        snapshot.workspace_roots,
        vec![cwd, PathBuf::from("/repo/extra")]
    );
    assert_eq!(
        snapshot.workspace_scopes[1],
        vec![PermissionRuleScope::User]
    );
}

#[test]
fn snapshot_build_normalizes_shorthand_path_rules_like_the_engine() {
    let _guard = setup();
    let cwd = PathBuf::from("/repo");
    // 引擎会把规则里的 `path:` 前缀按工作区根归一化；面板归因必须用同一形态，
    // 否则用户自己写的 `path:generated` 会被标成 ManagedOrCli 且无法删除。
    let engine = PermissionEngine::from_settings_and_flags(
        &cwd,
        Some(&json!({"allow": ["path:generated"]})),
        &PermissionFlags::default(),
    )
    .expect("engine");
    let policy_rules = engine.policy().rules;
    let layers = vec![PermissionSettingsLayer::new(
        PermissionRuleScope::User,
        Some(json!({"allow": ["path:generated"]})),
    )];
    let snapshot = PermissionSnapshot::build(PermissionSnapshotInput {
        cwd: cwd.clone(),
        mode: Some(PermissionMode::Default),
        bypass_disabled: false,
        policy_rules,
        workspace_roots: vec![cwd],
        session_allow_count: 0,
        layers,
        errors: Vec::new(),
    });
    assert_eq!(snapshot.rules.len(), 1, "{:?}", snapshot.rules);
    assert_eq!(snapshot.rules[0].scopes, vec![PermissionRuleScope::User]);
    assert!(snapshot.rules[0].removable);
}

#[test]
fn snapshot_build_marks_unattributed_entries_as_managed_or_cli() {
    let _guard = setup();
    let cwd = PathBuf::from("/repo");
    let rule = PermissionRule::tool("read", RuleDecision::Allow);
    let snapshot = PermissionSnapshot::build(PermissionSnapshotInput {
        cwd: cwd.clone(),
        mode: Some(PermissionMode::Default),
        bypass_disabled: false,
        policy_rules: vec![rule],
        workspace_roots: vec![cwd, PathBuf::from("/opt/extra")],
        session_allow_count: 0,
        layers: Vec::new(),
        errors: Vec::new(),
    });
    assert_eq!(
        snapshot.rules[0].scopes,
        vec![PermissionRuleScope::ManagedOrCli]
    );
    assert!(!snapshot.rules[0].removable);
    assert_eq!(
        snapshot.workspace_scopes[1],
        vec![PermissionRuleScope::ManagedOrCli]
    );
}

#[test]
fn snapshot_build_records_malformed_rules() {
    let _guard = setup();
    let cwd = PathBuf::from("/repo");
    let layers = vec![PermissionSettingsLayer::new(
        PermissionRuleScope::User,
        Some(json!({
            "rules": [{"tool": "read"}],
            "allow": [42],
            "addDirs": "not-an-array",
        })),
    )];
    let snapshot = PermissionSnapshot::build(PermissionSnapshotInput {
        cwd,
        mode: Some(PermissionMode::Default),
        bypass_disabled: false,
        policy_rules: Vec::new(),
        workspace_roots: Vec::new(),
        session_allow_count: 0,
        layers,
        errors: Vec::new(),
    });
    assert!(snapshot.rules.is_empty());
    assert_eq!(
        snapshot.errors.len(),
        2,
        "malformed rule + non-string allow entry"
    );
    // addDirs 形状错误不产生错误（引擎同样静默忽略）；根回退到 cwd。
    assert_eq!(snapshot.workspace_roots, vec![PathBuf::from("/repo")]);
}

#[test]
fn null_permissions_layer_is_treated_as_absent() {
    let _guard = setup();
    let cwd = PathBuf::from("/repo");
    let layers = vec![
        PermissionSettingsLayer::new(PermissionRuleScope::User, Some(serde_json::Value::Null)),
        PermissionSettingsLayer::new(PermissionRuleScope::Project, None),
    ];
    let snapshot = PermissionSnapshot::build(PermissionSnapshotInput {
        cwd,
        mode: Some(PermissionMode::Default),
        bypass_disabled: false,
        policy_rules: Vec::new(),
        workspace_roots: Vec::new(),
        session_allow_count: 0,
        layers,
        errors: Vec::new(),
    });
    assert!(snapshot.errors.is_empty(), "errors: {:?}", snapshot.errors);
    assert!(snapshot.rules.is_empty());
}

#[test]
fn collect_settings_layers_reads_global_project_and_local() {
    let _guard = setup();
    let cwd = tempfile::tempdir().expect("cwd");
    let agent = tempfile::tempdir().expect("agent");
    std::fs::write(
        agent.path().join("settings.json"),
        r#"{"permissions": {"rules": [{"tool": "read", "decision": "allow"}]}}"#,
    )
    .expect("global");
    let project_dir = cwd.path().join(".kiana");
    std::fs::create_dir_all(&project_dir).expect("project dir");
    std::fs::write(
        project_dir.join("settings.json"),
        r#"{"permissions": {"allow": ["command:git status"]}}"#,
    )
    .expect("project");
    std::fs::write(
        project_dir.join("settings.local.json"),
        r#"{"permissions": {"addDirs": ["/tmp/extra"]}}"#,
    )
    .expect("local");

    let manager = SettingsManager::create(
        &cwd.path().to_string_lossy(),
        &agent.path().to_string_lossy(),
    );
    let (layers, errors) = collect_settings_layers(&manager, cwd.path());
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    assert_eq!(layers.len(), 3);
    assert_eq!(layers[0].scope, PermissionRuleScope::User);
    assert_eq!(
        layers[0]
            .permissions
            .as_ref()
            .map(|value| &value["rules"][0]["tool"]),
        Some(&json!("read"))
    );
    assert_eq!(layers[1].scope, PermissionRuleScope::Project);
    assert_eq!(
        layers[1]
            .permissions
            .as_ref()
            .map(|value| &value["allow"][0]),
        Some(&json!("command:git status"))
    );
    assert_eq!(layers[2].scope, PermissionRuleScope::Local);
    assert_eq!(
        layers[2]
            .permissions
            .as_ref()
            .map(|value| &value["addDirs"][0]),
        Some(&json!("/tmp/extra"))
    );
}

#[test]
fn collect_settings_layers_skips_project_when_untrusted() {
    let _guard = setup();
    let cwd = tempfile::tempdir().expect("cwd");
    let agent = tempfile::tempdir().expect("agent");
    let project_dir = cwd.path().join(".kiana");
    std::fs::create_dir_all(&project_dir).expect("project dir");
    std::fs::write(
        project_dir.join("settings.json"),
        r#"{"permissions": {"allow": ["tool:read"]}}"#,
    )
    .expect("project");
    std::fs::write(
        project_dir.join("settings.local.json"),
        r#"{"permissions": {"allow": ["tool:write"]}}"#,
    )
    .expect("local");

    let manager = SettingsManager::create_with_options(
        &cwd.path().to_string_lossy(),
        &agent.path().to_string_lossy(),
        SettingsManagerCreateOptions {
            project_trusted: Some(false),
        },
    );
    let (layers, errors) = collect_settings_layers(&manager, cwd.path());
    assert!(errors.is_empty());
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].scope, PermissionRuleScope::User);
}

#[test]
fn display_rule_text_uses_stable_prefixes() {
    let cases = [
        (
            PermissionRule::tool("edit", RuleDecision::Allow),
            "tool:edit",
        ),
        (
            PermissionRule::command(vec!["cargo".into(), "test".into()], RuleDecision::Ask),
            "command:cargo test",
        ),
        (
            PermissionRule::path("/etc", RuleDecision::Deny),
            "path:/etc",
        ),
        (
            PermissionRule::exact_command("echo a; rm -rf b", RuleDecision::Allow),
            "exact:echo a; rm -rf b",
        ),
        (
            PermissionRule::wildcard_command("git *", RuleDecision::Allow),
            "wildcard:git *",
        ),
    ];
    for (rule, expected) in cases {
        assert_eq!(display_rule_text(&rule), expected);
    }
}

#[test]
fn build_snapshot_keeps_rule_reason_out_of_identity() {
    let _guard = setup();
    let cwd = PathBuf::from("/repo");
    let raw = json!({"tool": "edit", "decision": "allow", "reason": "because"});
    let rule = parse_settings_rule_value(&raw, &cwd).expect("rule");
    assert_eq!(
        components::permissions_dialog::rule_reason(&rule),
        Some("because")
    );
    let snapshot = PermissionSnapshot::build(PermissionSnapshotInput {
        cwd: cwd.clone(),
        mode: Some(PermissionMode::Default),
        bypass_disabled: false,
        policy_rules: vec![rule],
        workspace_roots: vec![cwd],
        session_allow_count: 0,
        layers: vec![PermissionSettingsLayer::new(
            PermissionRuleScope::User,
            Some(json!({"rules": [raw]})),
        )],
        errors: Vec::new(),
    });
    assert_eq!(
        snapshot.rules[0].raw,
        json!({"tool": "edit", "decision": "allow", "reason": "because"})
    );
}

// ---------------------------------------------------------------------------
// ApprovalModeIndicator
// ---------------------------------------------------------------------------

#[test]
fn indicator_none_hides_everything() {
    let _guard = setup();
    let indicator = ApprovalModeIndicator::new(None);
    assert!(!indicator.is_visible());
    assert!(indicator.text().is_none());
    assert!(indicator.segment().is_none());
    assert!(indicator.plain_segment().is_none());
    assert!(indicator.render().is_none());
    assert_eq!(indicator.plain_text(), None);
    assert_eq!(indicator.mode(), None);
    // 关闭态仍可给权限面板用。
    assert!(strip_ansi(&indicator.mode_line()).contains("Mode: off"));
}

#[test]
fn indicator_each_mode_has_text_and_color() {
    let _guard = setup();
    let expected = [
        (
            PermissionMode::ReadOnly,
            "read-only",
            ThemeColor::Success,
            false,
        ),
        (PermissionMode::Default, "default", ThemeColor::Dim, false),
        (
            PermissionMode::AcceptEdits,
            "accept-edits",
            ThemeColor::Warning,
            false,
        ),
        (
            PermissionMode::BypassPermissions,
            "bypass",
            ThemeColor::Error,
            true,
        ),
    ];
    for (mode, text, color, is_bold) in expected {
        let indicator = ApprovalModeIndicator::new(Some(mode));
        assert_eq!(indicator.text(), Some(text));
        assert_eq!(indicator.plain_segment(), Some(format!(" • {text}")));
        let visual = permission_mode_visual(mode);
        assert_eq!(
            visual,
            PermissionModeVisual {
                text,
                color,
                bold: is_bold
            }
        );
        let expected_styled = if is_bold {
            kiana_theme::THEME
                .bold(
                    &kiana_theme::THEME
                        .fg(color, text)
                        .expect("theme initialized"),
                )
                .expect("theme initialized")
        } else {
            kiana_theme::THEME
                .fg(color, text)
                .expect("theme initialized")
        };
        assert_eq!(indicator.styled_text(), Some(expected_styled));
        assert_eq!(
            indicator.segment(),
            Some(format!(" • {}", indicator.styled_text().expect("styled")))
        );
        assert_eq!(
            indicator.plain_text(),
            Some(format!("Mode: {text} (m to change)"))
        );
    }
}

#[test]
fn indicator_mode_line_tracks_mode() {
    let _guard = setup();
    let mut indicator = ApprovalModeIndicator::new(Some(PermissionMode::ReadOnly));
    assert!(strip_ansi(&indicator.mode_line()).contains("read-only"));
    indicator.set_mode(Some(PermissionMode::BypassPermissions));
    assert!(strip_ansi(&indicator.mode_line()).contains("bypass"));
    indicator.set_mode(None);
    assert!(strip_ansi(&indicator.mode_line()).contains("off"));
}

#[test]
fn indicator_exposes_footer_api_names() {
    let _guard = setup();
    let mut indicator = ApprovalModeIndicator::new(None);
    indicator.set_permission_mode(Some(PermissionMode::AcceptEdits));
    assert_eq!(
        indicator.permission_mode(),
        Some(PermissionMode::AcceptEdits)
    );
    assert_eq!(indicator.text(), Some("accept-edits"));
    indicator.set_permission_mode(None);
    assert_eq!(indicator.permission_mode(), None);
    assert!(indicator.segment().is_none());
}

#[test]
fn filtering_keeps_the_selected_rule_by_identity() {
    let _guard = setup();
    let snapshot = PermissionSnapshot {
        mode: Some(PermissionMode::Default),
        bypass_disabled: false,
        rules: ["edit", "read", "write"]
            .into_iter()
            .map(|tool| {
                entry(
                    PermissionRule::tool(tool, RuleDecision::Allow),
                    &[PermissionRuleScope::User],
                    true,
                )
            })
            .collect(),
        workspace_roots: vec![PathBuf::from("/repo")],
        workspace_scopes: vec![Vec::new()],
        session_allow_count: 0,
        errors: Vec::new(),
    };
    let mut dialog = dialog(snapshot, PermissionsCallbacks::default());
    // Allow 标签页的行：[Add, edit, read, write]；选中 read。
    down(&mut dialog);
    down(&mut dialog);
    assert_eq!(dialog.selected_rule(), 2);

    // 过滤 "r"：edit 被隐藏，可见行变成 [Add, read, write]。按位置夹取会把选中的
    // 下标 2 落到 write 上；必须按行身份还原到 read。
    type_text(&mut dialog, "/");
    type_text(&mut dialog, "r");
    assert_eq!(
        dialog.selected_rule(),
        1,
        "selection must follow the read rule, not the clamped position"
    );

    // 提交过滤后按 d：删除确认必须指向 read。
    enter(&mut dialog);
    dialog.handle_input("d");
    match dialog.view() {
        PermissionsView::DeleteRuleConfirm { entry, .. } => assert_eq!(
            entry.rule,
            PermissionRule::tool("read", RuleDecision::Allow)
        ),
        other => panic!("expected delete confirmation, got {other:?}"),
    }
}
