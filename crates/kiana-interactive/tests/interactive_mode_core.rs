//! `test/format-resume-command.test.ts` 与 `test/interactive-mode-bug-report-hint.test.ts`
//! 的移植（core 部分）。
//!
//! TS 用 `Object.defineProperty(process.stdout, "isTTY", …)` 覆盖 TTY 判定；Rust 的
//! `Stdout` 无法替换，因此 [`format_resume_command_with`] 把判定作为参数暴露给测试
//! （生产入口 [`format_resume_command`] 仍走 `std::io::IsTerminal`）。
//!
//! bug-report 的纯判定 `should_suggest_bug_report` 是 crate-private，对应用例在
//! `src/interactive_mode.rs` 的单元测试里（与 TS 的 `Reflect.get` 私有方法调用等价）。

mod common;

use kiana_core::config::APP_NAME;
use kiana_interactive::interactive_mode::{InteractiveMode, InteractiveModeOptions};
use kiana_interactive::interactive_mode::{
    ResumeCommandSource, format_crash_extension_hint, format_resume_command_with,
};

const SESSION_ID: &str = "0197f6e4-4cf9-7f44-a2d8-f8f7f49ee9d3";

#[derive(Default)]
struct FakeSessionManager {
    persisted: bool,
    persisted_set: bool,
    session_file: Option<String>,
    session_id: Option<String>,
    session_dir: Option<String>,
    uses_default_session_dir: Option<bool>,
}

impl FakeSessionManager {
    fn new() -> Self {
        FakeSessionManager {
            persisted: true,
            persisted_set: true,
            ..Default::default()
        }
    }

    fn persisted(mut self, persisted: bool) -> Self {
        self.persisted = persisted;
        self.persisted_set = true;
        self
    }

    fn session_file(mut self, file: Option<String>) -> Self {
        self.session_file = file;
        self
    }

    fn session_id(mut self, id: &str) -> Self {
        self.session_id = Some(id.to_string());
        self
    }

    fn session_dir(mut self, dir: &str) -> Self {
        self.session_dir = Some(dir.to_string());
        self
    }

    fn uses_default_session_dir(mut self, value: bool) -> Self {
        self.uses_default_session_dir = Some(value);
        self
    }
}

impl ResumeCommandSource for FakeSessionManager {
    fn is_persisted(&self) -> bool {
        self.persisted_set && self.persisted
    }

    fn get_session_file(&self) -> Option<String> {
        self.session_file.clone()
    }

    fn get_session_id(&self) -> String {
        self.session_id
            .clone()
            .unwrap_or_else(|| SESSION_ID.to_string())
    }

    fn get_session_dir(&self) -> String {
        self.session_dir
            .clone()
            .unwrap_or_else(|| "/tmp/kiana-sessions".to_string())
    }

    fn uses_default_session_dir(&self) -> bool {
        self.uses_default_session_dir.unwrap_or(true)
    }
}

fn create_temp_file() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("session.jsonl");
    std::fs::write(&file, "\n").expect("session file");
    (dir, file.to_string_lossy().into_owned())
}

#[test]
fn returns_a_session_resume_command_for_default_session_dirs() {
    let (_dir, session_file) = create_temp_file();
    let manager = FakeSessionManager::new()
        .session_file(Some(session_file))
        .session_id("test-session");
    assert_eq!(
        format_resume_command_with(&manager, true).as_deref(),
        Some(format!("{APP_NAME} --session test-session").as_str())
    );
}

#[test]
fn includes_unquoted_safe_session_dirs_for_non_default_session_dirs() {
    let (_dir, session_file) = create_temp_file();
    let manager = FakeSessionManager::new()
        .session_file(Some(session_file))
        .session_id("test-session")
        .session_dir("/tmp/custom-kiana-sessions")
        .uses_default_session_dir(false);
    assert_eq!(
        format_resume_command_with(&manager, true).as_deref(),
        Some(
            format!("{APP_NAME} --session-dir /tmp/custom-kiana-sessions --session test-session")
                .as_str()
        )
    );
}

#[test]
fn quotes_session_dirs_containing_spaces() {
    let (_dir, session_file) = create_temp_file();
    let manager = FakeSessionManager::new()
        .session_file(Some(session_file))
        .session_id("test-session")
        .session_dir("/tmp/custom kiana sessions")
        .uses_default_session_dir(false);
    assert_eq!(
        format_resume_command_with(&manager, true).as_deref(),
        Some(
            format!("{APP_NAME} --session-dir '/tmp/custom kiana sessions' --session test-session")
                .as_str()
        )
    );
}

#[test]
fn quotes_session_dirs_containing_single_quotes() {
    let (_dir, session_file) = create_temp_file();
    let manager = FakeSessionManager::new()
        .session_file(Some(session_file))
        .session_id("test-session")
        .session_dir("/tmp/custom kiana's sessions")
        .uses_default_session_dir(false);
    assert_eq!(
        format_resume_command_with(&manager, true).as_deref(),
        Some(
            format!(
                "{APP_NAME} --session-dir '/tmp/custom kiana'\\''s sessions' --session test-session"
            )
            .as_str()
        )
    );
}

#[test]
fn returns_none_when_stdout_is_not_a_tty() {
    let (_dir, session_file) = create_temp_file();
    let manager = FakeSessionManager::new().session_file(Some(session_file));
    assert_eq!(format_resume_command_with(&manager, false), None);
}

#[test]
fn returns_none_for_in_memory_sessions() {
    let (_dir, session_file) = create_temp_file();
    let manager = FakeSessionManager::new()
        .persisted(false)
        .session_file(Some(session_file));
    assert_eq!(format_resume_command_with(&manager, true), None);
}

#[test]
fn returns_none_when_the_session_file_is_missing() {
    let manager = FakeSessionManager::new()
        .session_file(Some("/tmp/kiana-missing-session.jsonl".to_string()));
    assert_eq!(format_resume_command_with(&manager, true), None);
}

#[test]
fn returns_none_when_the_session_file_is_not_set() {
    let manager = FakeSessionManager::new().session_file(None);
    assert_eq!(format_resume_command_with(&manager, true), None);
}

/// `interactive-mode-bug-report-hint.test.ts` case 1。
#[test]
fn identifies_extensions_with_frames_in_a_crash_stack() {
    assert_eq!(
        format_crash_extension_hint(Some(&["npm:kiana-observational-memory".to_string()]))
            .as_deref(),
        Some(
            "A stack frame came from loaded extension `npm:kiana-observational-memory`, which may be involved. Try disabling it with `kiana config`, or run `kiana -ne` to confirm."
        )
    );
    assert_eq!(format_crash_extension_hint(None), None);
}

/// 构造器冒烟测试：`InteractiveMode::new` 的字段/回调接线（不触发 `init()` 的
/// 网络下载与终端配色等待）。
#[tokio::test(flavor = "current_thread")]
async fn constructs_the_interactive_mode_and_stops() {
    common::run_local(async {
        let host = common::runtime_host(common::RuntimeHostOptions::new()).await;
        let terminal = common::TestTerminal::new();
        let mut mode = InteractiveMode::new(
            host.runtime,
            InteractiveModeOptions {
                terminal: Some(Box::new(terminal)),
                ..Default::default()
            },
        );
        assert!(!mode.is_initialized());
        assert!(!mode.session().session_id().is_empty());
        mode.stop();
        assert!(!mode.is_initialized());
    })
    .await;
}

/// 临时设置进程环境变量并在 drop 时恢复。
struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: 集成测试进程内串行使用；`KIANA_OFFLINE` 只影响工具下载与遥测。
        unsafe { std::env::set_var(key, value) };
        EnvGuard { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => unsafe { std::env::set_var(self.key, value) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

/// `init()` 回归（review critical #1）：
///
/// 首次 `programStatus.report()` 曾在上报器 getter 里重入 `InteractiveModeInner`
/// 的 `RefCell`（`init()` 正持有 `borrow_mut()`），导致每次启动 panic
/// `already mutably borrowed`。测试同时覆盖 critical #2：`init()` 里的
/// `debug_assert!(focused)` 要求编辑器代理地址能在挂载树里解析。
#[tokio::test(flavor = "current_thread")]
async fn init_starts_the_ui_without_refcell_reentrancy() {
    common::run_local(async {
        // 避免 `ensureTool("rg")` 在测试里真的联网下载。
        let _offline = EnvGuard::set("KIANA_OFFLINE", "1");
        let host = common::runtime_host(common::RuntimeHostOptions::new()).await;
        let terminal = common::TestTerminal::new();
        let program_status = terminal.program_status_handle();
        let mut mode = InteractiveMode::new(
            host.runtime,
            InteractiveModeOptions {
                terminal: Some(Box::new(terminal)),
                ..Default::default()
            },
        );
        mode.init().await;
        assert!(mode.is_initialized());
        assert!(
            !program_status.borrow().is_empty(),
            "init() must report the initial program status"
        );
        mode.stop();
        assert!(!mode.is_initialized());
    })
    .await;
}

/// 同样的 `init()` 回归，但强制 regular 渲染器（默认设置是 fullscreen）。
///
/// `set_focus_to_address` 的顶层挂载路径在两种渲染器里不同（fullscreen 的
/// `add_child` 会额外包一层代理），两种模式都必须能解析编辑器地址。
#[tokio::test(flavor = "current_thread")]
async fn init_focuses_the_editor_in_regular_mode() {
    common::run_local(async {
        let _offline = EnvGuard::set("KIANA_OFFLINE", "1");
        let host = common::runtime_host(common::RuntimeHostOptions::new()).await;
        let terminal = common::TestTerminal::new();
        let mut mode = InteractiveMode::new(
            host.runtime,
            InteractiveModeOptions {
                terminal: Some(Box::new(terminal)),
                tui_mode: Some(kiana_config::settings::TuiMode::Regular),
                ..Default::default()
            },
        );
        mode.init().await;
        assert!(mode.is_initialized());
        mode.stop();
        assert!(!mode.is_initialized());
    })
    .await;
}
