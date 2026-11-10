//! 移植 `session-selector-path-delete.test.ts`（8 项）与 `session-selector-rename.test.ts`（3 项）。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use kiana_ai::utils::abort::AbortScope;
use kiana_config::keybindings::{KeybindingsConfig, KeybindingsManager};
use kiana_interactive::components::session_selector::{
    DeleteMethod, SessionLoaderFuture, SessionSelectorComponent, SessionSelectorOptions,
    SessionsLoader, delete_session_file,
};
use kiana_session_core::{SessionInfo, SessionListProgress};
use kiana_tui::tui::Component;

const CTRL_D: &str = "\x04";
const CTRL_BACKSPACE: &str = "\x1b[127;5u";
const CTRL_R: &str = "\x1b[114;5u";

static THEME_INIT: Once = Once::new();

fn setup() {
    THEME_INIT.call_once(|| kiana_theme::init_theme(Some("dark"), false));
    kiana_tui::set_keybindings(kiana_tui::keybindings::KeybindingsManager::new(
        kiana_config::keybindings::keybindings().clone(),
        KeybindingsConfig::new(),
    ));
}

fn app_keybindings() -> KeybindingsManager {
    KeybindingsManager::new(KeybindingsConfig::new(), None)
}

fn options() -> SessionSelectorOptions {
    SessionSelectorOptions {
        keybindings: Some(app_keybindings()),
        ..Default::default()
    }
}

async fn flush() {
    tokio::time::sleep(Duration::from_millis(5)).await;
}

fn make_session(overrides: SessionSpec) -> SessionInfo {
    SessionInfo {
        path: overrides
            .path
            .unwrap_or_else(|| format!("/tmp/{}.jsonl", overrides.id)),
        id: overrides.id,
        cwd: overrides.cwd,
        name: overrides.name,
        parent_session_path: overrides.parent_session_path,
        created: 0.0,
        modified: overrides.modified,
        message_count: overrides.message_count,
        first_message: overrides.first_message,
        all_messages_text: overrides.all_messages_text,
    }
}

struct SessionSpec {
    id: String,
    path: Option<String>,
    name: Option<String>,
    parent_session_path: Option<String>,
    cwd: String,
    modified: f64,
    message_count: usize,
    first_message: String,
    all_messages_text: String,
}

impl SessionSpec {
    fn new(id: &str) -> Self {
        SessionSpec {
            id: id.to_string(),
            path: None,
            name: None,
            parent_session_path: None,
            cwd: String::new(),
            modified: 0.0,
            message_count: 1,
            first_message: "hello".to_string(),
            all_messages_text: "hello".to_string(),
        }
    }

    fn path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    fn name(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
    }

    fn parent(mut self, parent: impl Into<String>) -> Self {
        self.parent_session_path = Some(parent.into());
        self
    }

    fn modified(mut self, modified: f64) -> Self {
        self.modified = modified;
        self
    }
}

/// 立即返回固定会话列表的 loader。
fn immediate_loader(sessions: Vec<SessionInfo>) -> SessionsLoader {
    Arc::new(
        move |_progress: Option<SessionListProgress>, _signal: Option<AbortScope>| {
            let sessions = sessions.clone();
            Box::pin(async move { Ok(sessions) }) as SessionLoaderFuture
        },
    )
}

/// 用共享的 Notify/槽位模拟延迟 resolve 的 async loader。
#[derive(Clone)]
struct DeferredSessions {
    notify: Arc<tokio::sync::Notify>,
    slot: Arc<Mutex<Option<Vec<SessionInfo>>>>,
}

impl DeferredSessions {
    fn new() -> Self {
        DeferredSessions {
            notify: Arc::new(tokio::sync::Notify::new()),
            slot: Arc::new(Mutex::new(None)),
        }
    }

    fn resolve(&self, sessions: Vec<SessionInfo>) {
        *self.slot.lock().unwrap() = Some(sessions);
        self.notify.notify_waiters();
    }

    fn loader(&self) -> SessionsLoader {
        let notify = Arc::clone(&self.notify);
        let slot = Arc::clone(&self.slot);
        Arc::new(move |_progress, _signal| {
            let notify = Arc::clone(&notify);
            let slot = Arc::clone(&slot);
            Box::pin(async move {
                loop {
                    // 先注册再检查，避免 notify 丢失。
                    let notified = notify.notified();
                    if let Some(sessions) = slot.lock().unwrap().take() {
                        return Ok(sessions);
                    }
                    notified.await;
                }
            }) as SessionLoaderFuture
        })
    }
}

fn noop_select() -> Box<dyn FnMut(&str)> {
    Box::new(|_| {})
}

fn noop() -> Box<dyn FnMut()> {
    Box::new(|| {})
}

fn noop_render() -> Arc<dyn Fn() + Send + Sync> {
    Arc::new(|| {})
}

fn new_selector(
    current: SessionsLoader,
    all: SessionsLoader,
    options: SessionSelectorOptions,
    current_session_file_path: Option<String>,
) -> SessionSelectorComponent {
    SessionSelectorComponent::new(
        current,
        all,
        noop_select(),
        noop(),
        noop(),
        noop_render(),
        options,
        current_session_file_path,
    )
}

#[tokio::test]
async fn ctrl_backspace_is_not_delete_when_query_is_non_empty() {
    setup();
    let sessions = vec![
        make_session(SessionSpec::new("a")),
        make_session(SessionSpec::new("b")),
    ];
    let mut selector = new_selector(
        immediate_loader(sessions),
        immediate_loader(Vec::new()),
        options(),
        None,
    );
    flush().await;

    let changes: Rc<RefCell<Vec<Option<String>>>> = Rc::new(RefCell::new(Vec::new()));
    let changes_clone = Rc::clone(&changes);
    selector.get_session_list().on_delete_confirmation_change =
        Some(Box::new(move |path: Option<&str>| {
            changes_clone.borrow_mut().push(path.map(str::to_string));
        }));

    selector.get_session_list().handle_input("a");
    selector.get_session_list().handle_input(CTRL_BACKSPACE);

    assert!(changes.borrow().is_empty());
}

#[tokio::test]
async fn ctrl_d_enters_confirmation_even_with_non_empty_query() {
    setup();
    let sessions = vec![
        make_session(SessionSpec::new("a")),
        make_session(SessionSpec::new("b")),
    ];
    let expected = sessions[0].path.clone();
    let mut selector = new_selector(
        immediate_loader(sessions),
        immediate_loader(Vec::new()),
        options(),
        None,
    );
    flush().await;

    let changes: Rc<RefCell<Vec<Option<String>>>> = Rc::new(RefCell::new(Vec::new()));
    let changes_clone = Rc::clone(&changes);
    selector.get_session_list().on_delete_confirmation_change =
        Some(Box::new(move |path: Option<&str>| {
            changes_clone.borrow_mut().push(path.map(str::to_string));
        }));

    selector.get_session_list().handle_input("a");
    selector.get_session_list().handle_input(CTRL_D);

    assert_eq!(changes.borrow().as_slice(), [Some(expected)]);
}

#[tokio::test]
async fn ctrl_backspace_enters_confirmation_and_enter_deletes() {
    setup();
    let sessions = vec![
        make_session(SessionSpec::new("a")),
        make_session(SessionSpec::new("b")),
    ];
    let expected = sessions[0].path.clone();
    let mut selector = new_selector(
        immediate_loader(sessions),
        immediate_loader(Vec::new()),
        options(),
        None,
    );
    flush().await;

    let changes: Rc<RefCell<Vec<Option<String>>>> = Rc::new(RefCell::new(Vec::new()));
    let changes_clone = Rc::clone(&changes);
    selector.get_session_list().on_delete_confirmation_change =
        Some(Box::new(move |path: Option<&str>| {
            changes_clone.borrow_mut().push(path.map(str::to_string));
        }));
    let deleted: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let deleted_clone = Rc::clone(&deleted);
    selector.get_session_list().on_delete_session = Some(Box::new(move |path: &str| {
        *deleted_clone.borrow_mut() = Some(path.to_string());
    }));

    selector.get_session_list().handle_input(CTRL_BACKSPACE);
    assert_eq!(changes.borrow().as_slice(), [Some(expected.clone())]);

    selector.get_session_list().handle_input("\r");
    assert_eq!(changes.borrow().as_slice(), [Some(expected.clone()), None]);
    assert_eq!(deleted.borrow().as_deref(), Some(expected.as_str()));
}

#[tokio::test]
async fn all_scope_load_resolving_late_does_not_switch_back_to_all() {
    setup();
    let current_sessions = vec![make_session(SessionSpec::new("current"))];
    let deferred = DeferredSessions::new();
    let all_calls = Arc::new(AtomicUsize::new(0));
    let all_calls_clone = Arc::clone(&all_calls);
    let deferred_loader = deferred.loader();
    let all_loader: SessionsLoader = Arc::new(move |progress, signal| {
        all_calls_clone.fetch_add(1, Ordering::SeqCst);
        deferred_loader(progress, signal)
    });

    let mut selector = new_selector(
        immediate_loader(current_sessions),
        all_loader,
        options(),
        None,
    );
    flush().await;

    let list = selector.get_session_list();
    list.handle_input("\t");
    list.handle_input("\t");

    deferred.resolve(vec![make_session(SessionSpec::new("all"))]);
    flush().await;

    assert_eq!(all_calls.load(Ordering::SeqCst), 1);
    let output = selector.render(120).join("\n");
    assert!(output.contains("Resume Session (Current Folder)"));
    assert!(!output.contains("Resume Session (All)"));
}

#[tokio::test]
async fn toggling_scopes_does_not_start_redundant_all_loads() {
    setup();
    let current_sessions = vec![make_session(SessionSpec::new("current"))];
    let all_sessions = vec![make_session(SessionSpec::new("all"))];
    let deferred = DeferredSessions::new();
    let all_calls = Arc::new(AtomicUsize::new(0));
    let all_calls_clone = Arc::clone(&all_calls);
    let progress_sessions = all_sessions.clone();
    let deferred_loader = deferred.loader();
    let all_loader: SessionsLoader = Arc::new(move |progress, signal| {
        all_calls_clone.fetch_add(1, Ordering::SeqCst);
        if let Some(progress) = &progress {
            progress(1, 2, Some(progress_sessions.clone()));
        }
        deferred_loader(progress, signal)
    });

    let mut selector = new_selector(
        immediate_loader(current_sessions),
        all_loader,
        options(),
        None,
    );
    flush().await;

    selector.get_session_list().handle_input("\t");
    selector.get_session_list().handle_input("\t");
    selector.get_session_list().handle_input("\t");

    assert_eq!(all_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        selector
            .get_session_list()
            .get_selected_session_path()
            .map(str::to_string),
        Some(all_sessions[0].path.clone())
    );
    assert!(selector.render(120).join("\n").contains("Loading"));

    deferred.resolve(all_sessions);
    flush().await;
}

#[cfg(unix)]
fn create_symlinked_session_paths() -> (tempfile::TempDir, String, String, String) {
    use std::fs;
    use std::os::unix::fs::symlink;

    let base_dir = tempfile::tempdir().expect("temp dir");
    let real_dir = base_dir.path().join("real");
    let alias_a_dir = base_dir.path().join("alias-a");
    let alias_b_dir = base_dir.path().join("alias-b");
    fs::create_dir_all(&real_dir).expect("real dir");
    fs::create_dir_all(&alias_a_dir).expect("alias a dir");
    fs::create_dir_all(&alias_b_dir).expect("alias b dir");

    let shared_dir = real_dir.join("sessions");
    fs::create_dir_all(&shared_dir).expect("shared dir");
    let alias_a_sessions = alias_a_dir.join("sessions");
    let alias_b_sessions = alias_b_dir.join("sessions");
    symlink(&shared_dir, &alias_a_sessions).expect("symlink a");
    symlink(&shared_dir, &alias_b_sessions).expect("symlink b");

    fs::write(shared_dir.join("parent.jsonl"), "parent\n").expect("parent file");
    fs::write(shared_dir.join("child.jsonl"), "child\n").expect("child file");

    let parent_alias_a = alias_a_sessions.join("parent.jsonl");
    let parent_alias_b = alias_b_sessions.join("parent.jsonl");
    let child_alias_b = alias_b_sessions.join("child.jsonl");
    (
        base_dir,
        parent_alias_a.to_string_lossy().into_owned(),
        parent_alias_b.to_string_lossy().into_owned(),
        child_alias_b.to_string_lossy().into_owned(),
    )
}

#[cfg(unix)]
#[tokio::test]
async fn threads_sessions_when_parent_and_child_paths_use_different_symlink_aliases() {
    setup();
    let (_base_dir, parent_alias_a, parent_alias_b, child_alias_b) =
        create_symlinked_session_paths();
    let sessions = vec![
        make_session(
            SessionSpec::new("parent")
                .path(parent_alias_b)
                .name("Parent")
                .modified(date_ms(2026, 1, 1)),
        ),
        make_session(
            SessionSpec::new("child")
                .path(child_alias_b)
                .parent(parent_alias_a)
                .name("Child")
                .modified(date_ms(2025, 12, 31)),
        ),
    ];
    let mut selector = new_selector(
        immediate_loader(sessions),
        immediate_loader(Vec::new()),
        options(),
        None,
    );
    flush().await;

    let output = strip_ansi(&selector.render(120).join("\n"));
    assert!(output.contains("Parent"));
    assert!(output.contains("└─ Child"));
}

#[tokio::test]
async fn sorts_threaded_sessions_by_latest_activity_in_their_subtree() {
    setup();
    let parent_one = make_session(
        SessionSpec::new("parent-one")
            .name("Parent one")
            .modified(date_ms(2026, 1, 2)),
    );
    let parent_two_path = "/tmp/parent-two.jsonl".to_string();
    let parent_two = make_session(
        SessionSpec::new("parent-two")
            .name("Parent two")
            .modified(date_ms(2026, 1, 1)),
    );
    let child_two = make_session(
        SessionSpec::new("child-two")
            .name("Child two")
            .parent(parent_two_path)
            .modified(date_ms(2026, 1, 3)),
    );
    let mut selector = new_selector(
        immediate_loader(vec![parent_one, parent_two, child_two]),
        immediate_loader(Vec::new()),
        options(),
        None,
    );
    flush().await;

    let output = strip_ansi(&selector.render(120).join("\n"));
    let parent_two_index = output.find("Parent two").expect("parent two rendered");
    let child_two_index = output.find("└─ Child two").expect("child two rendered");
    let parent_one_index = output.find("Parent one").expect("parent one rendered");
    assert!(parent_two_index < child_two_index);
    assert!(child_two_index < parent_one_index);
}

#[cfg(unix)]
#[tokio::test]
async fn treats_current_session_as_active_across_symlink_aliases() {
    setup();
    let (_base_dir, parent_alias_a, parent_alias_b, _child_alias_b) =
        create_symlinked_session_paths();
    let sessions = vec![make_session(
        SessionSpec::new("parent")
            .path(parent_alias_b)
            .name("Parent"),
    )];
    let mut selector = new_selector(
        immediate_loader(sessions),
        immediate_loader(Vec::new()),
        options(),
        Some(parent_alias_a),
    );
    flush().await;

    let changes: Rc<RefCell<Vec<Option<String>>>> = Rc::new(RefCell::new(Vec::new()));
    let changes_clone = Rc::clone(&changes);
    selector.get_session_list().on_delete_confirmation_change =
        Some(Box::new(move |path: Option<&str>| {
            changes_clone.borrow_mut().push(path.map(str::to_string));
        }));
    let error: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let error_clone = Rc::clone(&error);
    selector.get_session_list().on_error = Some(Box::new(move |message: &str| {
        *error_clone.borrow_mut() = Some(message.to_string());
    }));

    selector.get_session_list().handle_input(CTRL_D);

    assert!(changes.borrow().is_empty());
    assert_eq!(
        error.borrow().as_deref(),
        Some("Cannot delete the currently active session")
    );
}

#[tokio::test]
async fn shows_rename_hint_in_interactive_picker_configuration() {
    setup();
    let sessions = vec![make_session(SessionSpec::new("a"))];
    let mut selector = new_selector(
        immediate_loader(sessions),
        immediate_loader(Vec::new()),
        SessionSelectorOptions {
            show_rename_hint: Some(true),
            keybindings: Some(app_keybindings()),
            ..Default::default()
        },
        None,
    );
    flush().await;

    let output = selector.render(120).join("\n");
    assert!(output.contains("ctrl+r"));
    assert!(output.contains("rename"));
}

#[tokio::test]
async fn does_not_show_rename_hint_in_resume_picker_configuration() {
    setup();
    let sessions = vec![make_session(SessionSpec::new("a"))];
    let mut selector = new_selector(
        immediate_loader(sessions),
        immediate_loader(Vec::new()),
        SessionSelectorOptions {
            show_rename_hint: Some(false),
            keybindings: Some(app_keybindings()),
            ..Default::default()
        },
        None,
    );
    flush().await;

    let output = selector.render(120).join("\n");
    assert!(!output.contains("ctrl+r"));
    assert!(!output.contains("rename"));
}

#[tokio::test]
async fn enters_rename_mode_on_ctrl_r_and_submits_with_enter() {
    setup();
    let session = make_session(SessionSpec::new("a").name("Old"));
    let session_path = session.path.clone();
    let calls: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let calls_clone = Rc::clone(&calls);
    let mut selector = new_selector(
        immediate_loader(vec![session]),
        immediate_loader(Vec::new()),
        SessionSelectorOptions {
            rename_session: Some(Box::new(move |path: &str, name: &str| {
                calls_clone
                    .borrow_mut()
                    .push((path.to_string(), name.to_string()));
            })),
            show_rename_hint: Some(true),
            keybindings: Some(app_keybindings()),
        },
        None,
    );
    flush().await;

    selector.get_session_list().handle_input(CTRL_R);
    flush().await;

    let output = selector.render(120).join("\n");
    assert!(output.contains("Rename Session"));
    assert!(!output.contains("Resume Session"));

    selector.handle_input("X");
    selector.handle_input("\r");
    flush().await;

    assert_eq!(
        calls.borrow().as_slice(),
        [(session_path, "XOld".to_string())]
    );
}

/// TS 的 `onRenameSession` 在 list.handleInput 内同步进入 rename 模式：直接驱动
/// `getSessionList().handleInput(ctrl+r)` 之后，下一次组件按键就应写进 rename 输入框。
#[tokio::test]
async fn rename_mode_enters_synchronously_after_direct_list_input() {
    setup();
    let session = make_session(SessionSpec::new("a").name("Old"));
    let session_path = session.path.clone();
    let calls: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let calls_clone = Rc::clone(&calls);
    let mut selector = new_selector(
        immediate_loader(vec![session]),
        immediate_loader(Vec::new()),
        SessionSelectorOptions {
            rename_session: Some(Box::new(move |path: &str, name: &str| {
                calls_clone
                    .borrow_mut()
                    .push((path.to_string(), name.to_string()));
            })),
            show_rename_hint: Some(true),
            keybindings: Some(app_keybindings()),
        },
        None,
    );
    flush().await;

    // 不经过 render：模拟宿主把按键直接交给列表对象。
    selector.get_session_list().handle_input(CTRL_R);
    // TS 里组件此刻已经在 rename 模式，这个按键应作为文本输入（光标在开头）。
    selector.handle_input("X");
    selector.handle_input("\r");
    flush().await;

    assert_eq!(
        calls.borrow().as_slice(),
        [(session_path, "XOld".to_string())]
    );
}

fn strip_ansi(text: &str) -> String {
    kiana_core::ansi::strip_ansi(text)
}

fn date_ms(year: i32, month: u32, day: u32) -> f64 {
    use chrono::TimeZone;
    chrono::Utc
        .with_ymd_and_hms(year, month, day, 0, 0, 0)
        .single()
        .expect("valid test date")
        .timestamp_millis() as f64
}

#[tokio::test]
async fn renders_ts_golden_session_selector_layout() {
    setup();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        * 1000.0;
    let named = SessionInfo {
        path: "/tmp/named.jsonl".to_string(),
        id: "named".to_string(),
        cwd: String::new(),
        name: Some("Named session".to_string()),
        parent_session_path: None,
        created: 0.0,
        modified: now - 90.0 * 60.0 * 1000.0,
        message_count: 3,
        first_message: "named first".to_string(),
        all_messages_text: "named first".to_string(),
    };
    let plain = SessionInfo {
        path: "/tmp/plain.jsonl".to_string(),
        id: "plain".to_string(),
        cwd: "/home/user/projects/kiana".to_string(),
        name: None,
        parent_session_path: None,
        created: 0.0,
        modified: now - 2.0 * 24.0 * 60.0 * 60.0 * 1000.0,
        message_count: 5,
        first_message: "hello\nworld".to_string(),
        all_messages_text: "hello\nworld".to_string(),
    };
    let mut selector = new_selector(
        immediate_loader(vec![named, plain]),
        immediate_loader(Vec::new()),
        options(),
        Some("/tmp/named.jsonl".to_string()),
    );
    flush().await;

    let expected = r#"== current 80 ==

────────────────────────────────────────────────────────────────────────────────

Resume Session (Current Fold ◉ Current Folder | ○ All  Name: All  Sort: Threaded
tab scope · re:<pattern> regex · "phrase" exact
ctrl+s sort · ctrl+n named · ctrl+d delete · ctrl+p path (off)

> _kiana:fc _kiana:/fc                                                                             

› Named session                                                             3 1h
  hello world                                                               5 2d

────────────────────────────────────────────────────────────────────────────────
== current 40 ==

────────────────────────────────────────

◉ Current Folder | ○ All  Name: All  Sor
tab scope · re:<pattern> regex · "phras…
ctrl+s sort · ctrl+n named · ctrl+d del…

> _kiana:fc _kiana:/fc                                     

› Named session                     3 1h
  hello world                       5 2d

────────────────────────────────────────
== toggled 80 ==

────────────────────────────────────────────────────────────────────────────────

Resume Session (Current Fold ◉ Current Folder | ○ All  Name: Named  Sort: Recent
tab scope · re:<pattern> regex · "phrase" exact
ctrl+s sort · ctrl+n named · ctrl+d delete · ctrl+p path (on)

> _kiana:fc _kiana:/fc                                                                             

› Named session                                            /tmp/named.jsonl 3 1h

────────────────────────────────────────────────────────────────────────────────
== all scope empty 80 ==

────────────────────────────────────────────────────────────────────────────────

Resume Session (All)           ○ Current Folder | ◉ All  Name: All  Sort: Recent
tab scope · re:<pattern> regex · "phrase" exact
ctrl+s sort · ctrl+n named · ctrl+d delete · ctrl+p path (on)

> _kiana:fc _kiana:/fc                                                                             

  No sessions found

────────────────────────────────────────────────────────────────────────────────
== empty 80 ==

────────────────────────────────────────────────────────────────────────────────

Resume Session (Current Fold ◉ Current Folder | ○ All  Name: All  Sort: Threaded
tab scope · re:<pattern> regex · "phrase" exact
ctrl+s sort · ctrl+n named · ctrl+d delete · ctrl+p path (off)

> _kiana:fc _kiana:/fc                                                                             

  No sessions in current folder. Press Tab to view all.

────────────────────────────────────────────────────────────────────────────────
== failing 80 ==

────────────────────────────────────────────────────────────────────────────────

Resume Session (Current Fold ◉ Current Folder | ○ All  Name: All  Sort: Threaded
Failed to load sessions: boom


> _kiana:fc _kiana:/fc                                                                             

  No sessions in current folder. Press Tab to view all.

────────────────────────────────────────────────────────────────────────────────
"#;

    let mut out = String::new();
    out.push_str("== current 80 ==\n");
    out.push_str(
        &selector
            .render(80)
            .into_iter()
            .map(|line| strip_ansi(&line))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    out.push('\n');
    out.push_str("== current 40 ==\n");
    out.push_str(
        &selector
            .render(40)
            .into_iter()
            .map(|line| strip_ansi(&line))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    out.push('\n');
    selector.get_session_list().handle_input("\x10");
    selector.get_session_list().handle_input("\x13");
    selector.get_session_list().handle_input("\x0e");
    out.push_str("== toggled 80 ==\n");
    out.push_str(
        &selector
            .render(80)
            .into_iter()
            .map(|line| strip_ansi(&line))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    out.push('\n');
    selector.get_session_list().handle_input("\x0e");
    out.push_str("== all scope empty 80 ==\n");
    selector.get_session_list().handle_input("\t");
    flush().await;
    out.push_str(
        &selector
            .render(80)
            .into_iter()
            .map(|line| strip_ansi(&line))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    out.push('\n');

    let mut empty_selector = new_selector(
        immediate_loader(Vec::new()),
        immediate_loader(Vec::new()),
        options(),
        None,
    );
    flush().await;
    out.push_str("== empty 80 ==\n");
    out.push_str(
        &empty_selector
            .render(80)
            .into_iter()
            .map(|line| strip_ansi(&line))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    out.push('\n');

    let failing_loader: SessionsLoader = Arc::new(|_, _| {
        Box::pin(async move {
            Err(kiana_session_core::SessionListError {
                name: "Error".to_string(),
                message: "boom".to_string(),
            })
        })
    });
    let mut failing = new_selector(
        failing_loader,
        immediate_loader(Vec::new()),
        options(),
        None,
    );
    flush().await;
    out.push_str("== failing 80 ==\n");
    out.push_str(
        &failing
            .render(80)
            .into_iter()
            .map(|line| strip_ansi(&line))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    out.push('\n');

    assert_eq!(out, expected);
}

// ---------------------------------------------------------------------------
// 回归测试：reviewer 发现的行为差异
// ---------------------------------------------------------------------------

/// 同一规范化路径的多个会话：TS 的 `buildSessionTree` 对每个会话 push `Map` 里的
/// **最后一个**节点，所以最后一条渲染两次、更早的一条被丢弃。
#[cfg(unix)]
#[tokio::test]
async fn duplicate_canonical_paths_render_last_session_twice() {
    setup();
    let (_base_dir, alias_a, alias_b, _child) = create_symlinked_session_paths();
    let sessions = vec![
        make_session(SessionSpec::new("first").path(alias_a).name("First alias")),
        make_session(
            SessionSpec::new("second")
                .path(alias_b)
                .name("Second alias"),
        ),
    ];
    let mut selector = new_selector(
        immediate_loader(sessions),
        immediate_loader(Vec::new()),
        options(),
        None,
    );
    flush().await;

    let output = strip_ansi(&selector.render(100).join("\n"));
    assert_eq!(output.matches("Second alias").count(), 2, "{output}");
    assert!(!output.contains("First alias"), "{output}");
}

/// 非有限 `modified`（NaN / ±Infinity）在 TS 里构造 Invalid Date，`getTime()` 为 NaN，
/// 日期列渲染 `NaNy`（而不是 JSON 的 `null`，也不是 `now`）。
#[tokio::test]
async fn formats_nan_and_infinite_session_dates_like_js() {
    setup();
    let sessions = vec![
        make_session(SessionSpec::new("nan").modified(f64::NAN)),
        make_session(SessionSpec::new("inf").modified(f64::INFINITY)),
    ];
    let mut selector = new_selector(
        immediate_loader(sessions),
        immediate_loader(Vec::new()),
        options(),
        None,
    );
    flush().await;

    let output = strip_ansi(&selector.render(100).join("\n"));
    assert_eq!(output.matches("1 NaNy").count(), 2, "{output}");
    assert!(!output.contains("nully"), "{output}");
}

/// `SessionList` 的搜索框 `onSubmit`：Input 把字面 "\n" 也当提交（Kitty shift+enter），
/// TS 会选中当前会话。
#[tokio::test]
async fn literal_newline_submits_and_selects_highlighted_session() {
    setup();
    let session = make_session(SessionSpec::new("a"));
    let expected = session.path.clone();
    let selected: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let selected_clone = Rc::clone(&selected);
    let mut selector = SessionSelectorComponent::new(
        immediate_loader(vec![session]),
        immediate_loader(Vec::new()),
        Box::new(move |path: &str| {
            *selected_clone.borrow_mut() = Some(path.to_string());
        }),
        noop(),
        noop(),
        noop_render(),
        options(),
        None,
    );
    flush().await;

    selector.get_session_list().handle_input("\n");
    assert_eq!(selected.borrow().as_deref(), Some(expected.as_str()));
}

/// rename 模式的提交同样走 `Input.onSubmit`（`tui.input.submit` / 字面 "\n"）。
#[tokio::test]
async fn literal_newline_submits_rename_input() {
    setup();
    let session = make_session(SessionSpec::new("a").name("Old"));
    let calls: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let calls_clone = Rc::clone(&calls);
    let mut selector = new_selector(
        immediate_loader(vec![session]),
        immediate_loader(Vec::new()),
        SessionSelectorOptions {
            rename_session: Some(Box::new(move |path: &str, name: &str| {
                calls_clone
                    .borrow_mut()
                    .push((path.to_string(), name.to_string()));
            })),
            show_rename_hint: Some(true),
            keybindings: Some(app_keybindings()),
        },
        None,
    );
    flush().await;

    selector.get_session_list().handle_input(CTRL_R);
    flush().await;
    // 宿主在输入后会请求重绘；组件在下一次 render 时进入 rename 模式。
    selector.render(80);
    selector.handle_input("X");
    selector.handle_input("\n");
    flush().await;

    assert_eq!(calls.borrow().len(), 1);
    assert_eq!(calls.borrow()[0].1, "XOld");
}

/// `confirmRename` 等待刷新加载完成才退出 rename 面板（TS 的 `finally` 时机）。
#[tokio::test]
async fn rename_panel_stays_visible_until_refresh_load_resolves() {
    setup();
    let initial = vec![make_session(SessionSpec::new("a").name("Old"))];
    let deferred = DeferredSessions::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_clone = Arc::clone(&calls);
    let deferred_loader = deferred.loader();
    let current_loader: SessionsLoader = Arc::new(move |progress, signal| {
        if calls_clone.fetch_add(1, Ordering::SeqCst) == 0 {
            let sessions = initial.clone();
            Box::pin(async move { Ok(sessions) }) as SessionLoaderFuture
        } else {
            deferred_loader(progress, signal)
        }
    });
    let mut selector = new_selector(
        current_loader,
        immediate_loader(Vec::new()),
        SessionSelectorOptions {
            rename_session: Some(Box::new(|_, _| {})),
            show_rename_hint: Some(true),
            keybindings: Some(app_keybindings()),
        },
        None,
    );
    flush().await;

    selector.get_session_list().handle_input(CTRL_R);
    flush().await;
    assert!(strip_ansi(&selector.render(80).join("\n")).contains("Rename Session"));

    selector.handle_input("X");
    selector.handle_input("\r");
    let output = strip_ansi(&selector.render(80).join("\n"));
    assert!(output.contains("Rename Session"), "{output}");

    deferred.resolve(vec![make_session(SessionSpec::new("a").name("New"))]);
    flush().await;
    let output = strip_ansi(&selector.render(80).join("\n"));
    assert!(!output.contains("Rename Session"), "{output}");
    assert!(output.contains("Resume Session"), "{output}");
}

/// 没有 `onDeleteSession` 时确认删除是 no-op（TS `void this.onDeleteSession?.(path)`）。
#[tokio::test]
async fn delete_confirmation_without_callback_is_a_noop() {
    setup();
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("session.jsonl");
    std::fs::write(&path, "x").expect("write session");
    let session = make_session(SessionSpec::new("a").path(path.to_string_lossy().into_owned()));
    let mut selector = new_selector(
        immediate_loader(vec![session]),
        immediate_loader(Vec::new()),
        options(),
        None,
    );
    flush().await;

    selector.get_session_list().on_delete_session = None;
    selector.get_session_list().handle_input(CTRL_D);
    selector.get_session_list().handle_input("\r");

    assert!(path.exists(), "file must not be deleted without a callback");
}

/// 覆盖 `onRenameSession` 会抑制组件内置的进入 rename 模式行为。
#[tokio::test]
async fn overriding_rename_callback_suppresses_builtin_rename_mode() {
    setup();
    let session = make_session(SessionSpec::new("a").name("Old"));
    let expected = session.path.clone();
    let requested: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let requested_clone = Rc::clone(&requested);
    let mut selector = new_selector(
        immediate_loader(vec![session]),
        immediate_loader(Vec::new()),
        SessionSelectorOptions {
            rename_session: Some(Box::new(|_, _| {})),
            show_rename_hint: Some(true),
            keybindings: Some(app_keybindings()),
        },
        None,
    );
    flush().await;

    selector.get_session_list().on_rename_session = Some(Box::new(move |path: &str| {
        *requested_clone.borrow_mut() = Some(path.to_string());
    }));
    selector.get_session_list().handle_input(CTRL_R);

    assert_eq!(requested.borrow().as_deref(), Some(expected.as_str()));
    let output = strip_ansi(&selector.render(80).join("\n"));
    assert!(!output.contains("Rename Session"), "{output}");
    assert!(output.contains("Resume Session"), "{output}");
}

/// `trash` 不存在且文件也不存在时按成功处理（TS: `!existsSync(sessionPath)`）。
#[test]
fn delete_missing_file_reports_trash_success() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("missing.jsonl");
    let result = kiana_interactive::components::session_selector::delete_session_file(
        &path.to_string_lossy(),
    );
    assert!(result.ok);
    assert_eq!(
        result.method,
        kiana_interactive::components::session_selector::DeleteMethod::Trash
    );
}

/// 删除失败时的文案按 Node 的 errno 格式拼装，而不是 Rust `io::Error` 的 Display。
#[cfg(unix)]
#[test]
fn delete_failure_reports_node_style_error_text() {
    // `trash` 存在时它会成功删除目录，测试不适用（本仓库的 CI 环境没有 trash）。
    if std::process::Command::new("trash")
        .arg("--version")
        .output()
        .is_ok()
    {
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("as-directory");
    std::fs::create_dir(&path).expect("mkdir");
    let result = delete_session_file(&path.to_string_lossy());
    assert!(!result.ok);
    assert_eq!(result.method, DeleteMethod::Unlink);
    let expected = format!(
        "EISDIR: illegal operation on a directory, unlink '{}' (trash: spawnSync trash ENOENT)",
        path.to_string_lossy()
    );
    assert_eq!(result.error.as_deref(), Some(expected.as_str()));
}

/// 回调缺失时不再用 core 兜底（TS 的 `this.onToggleSort?.()` 是 no-op）。
#[tokio::test]
async fn toggle_callbacks_are_noop_when_absent() {
    setup();
    let sessions = vec![make_session(SessionSpec::new("a"))];
    let mut selector = new_selector(
        immediate_loader(sessions),
        immediate_loader(Vec::new()),
        options(),
        None,
    );
    flush().await;

    selector.get_session_list().on_toggle_sort = None;
    selector.get_session_list().handle_input("\x13"); // ctrl+s
    let output = strip_ansi(&selector.render(120).join("\n"));
    assert!(output.contains("Sort: Threaded"), "{output}");
}
