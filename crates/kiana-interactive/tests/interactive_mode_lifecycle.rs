//! 生命周期 / 信号 / 输入 / 剪贴板（§2.50.7 / §2.50.8）。
//!
//! `5080` / `5724` 的完整信号路径需要真实 SIGTERM 投递与子进程退出码观察；这里覆盖
//! 可注入的进程退出与终端停止顺序，信号注册本身见 `register_signal_handlers`。

#![allow(
    clippy::arc_with_non_send_sync,
    clippy::await_holding_lock,
    reason = "单线程 LocalSet 测试：扩展工厂没有 Send/Sync 约束，且不存在并发借用者"
)]

mod common;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard};

use std::sync::Arc;

use kiana_config::settings::TuiMode;
use kiana_core::image_process::ClipboardImage;
use kiana_ext_api::types::ExtensionFactory;
use kiana_interactive::interactive_mode::input::{ClipboardSource, clipboard_paste_text};
use kiana_interactive::interactive_mode::lifecycle::{
    CtrlZPlatform, set_exit_hook_for_test, set_stdout_hook_for_test,
};
use kiana_interactive::interactive_mode::{InteractiveMode, InteractiveModeOptions};
use kiana_tui::tui::TuiMode as RendererTuiMode;

static THEME_LOCK: Mutex<()> = Mutex::new(());

fn theme() -> MutexGuard<'static, ()> {
    let guard = THEME_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    kiana_theme::init_theme(Some("dark"), false);
    guard
}

struct Fixture {
    mode: InteractiveMode,
    output: Rc<RefCell<String>>,
    _faux: kiana_ai::providers::faux::FauxCore,
    _temp: tempfile::TempDir,
    _engine: common::TestEngine,
}

async fn fixture() -> Fixture {
    fixture_with(|_| None).await
}

/// 允许测试在构造运行时之前拿到终端输出句柄（扩展处理器/断言共用）。
async fn fixture_with(
    extension_factory: impl FnOnce(Rc<RefCell<String>>) -> Option<ExtensionFactory>,
) -> Fixture {
    unsafe { std::env::set_var("KIANA_OFFLINE", "1") };
    let terminal = common::TestTerminal::new();
    let output = terminal.output_handle();
    let factory = extension_factory(Rc::clone(&output));
    let host = common::runtime_host(common::RuntimeHostOptions {
        extension_factory: factory,
        ..Default::default()
    })
    .await;
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
            terminal: Some(Box::new(terminal)),
            tui_mode: Some(TuiMode::Regular),
            ..Default::default()
        },
    );
    Fixture {
        mode,
        output,
        _faux: faux,
        _temp: temp_dir,
        _engine: engine,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn interactive_shutdown_stops_terminal_and_is_reentrant() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        let exits: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&exits);
        set_exit_hook_for_test(Some(Box::new(move |code| {
            recorded.borrow_mut().push(code);
        })));

        fixture.mode.test_shutdown(false).await;
        // 重入是 no-op（只退出一次）。
        fixture.mode.test_shutdown(false).await;
        assert_eq!(&*exits.borrow(), &[0]);
        assert!(!fixture.mode.is_initialized());
        set_exit_hook_for_test(None);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn switch_tui_mode_replaces_the_renderer_and_restores_the_previous_screen() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        let exits: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&exits);
        set_exit_hook_for_test(Some(Box::new(move |code| {
            recorded.borrow_mut().push(code);
        })));

        assert_eq!(fixture.mode.test_tui_mode(), RendererTuiMode::Regular);
        assert!(
            fixture
                .mode
                .test_switch_tui_mode(RendererTuiMode::Fullscreen, false)
        );
        assert_eq!(fixture.mode.test_tui_mode(), RendererTuiMode::Fullscreen);
        // 同一模式是 no-op。
        assert!(
            fixture
                .mode
                .test_switch_tui_mode(RendererTuiMode::Fullscreen, false)
        );
        // 回到主屏：复用同一个终端（TS 断言 start/stop 计数与屏幕恢复）。
        assert!(
            fixture
                .mode
                .test_switch_tui_mode(RendererTuiMode::Regular, false)
        );
        assert_eq!(fixture.mode.test_tui_mode(), RendererTuiMode::Regular);

        fixture.mode.test_shutdown(false).await;
        assert_eq!(&*exits.borrow(), &[0]);
        set_exit_hook_for_test(None);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn switch_tui_mode_keeps_the_focused_component_identity() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        fixture.mode.test_show_settings_selector();
        common::yield_tasks(10).await;
        let focused = fixture
            .mode
            .test_ui_focus_identity()
            .expect("the settings list must be focused");

        // regular -> fullscreen -> regular：TS 的 `nextUi.setFocus(getFocusedComponent())`
        // 保留的是组件引用。Rust 必须按组件地址重新解析，选择器在两种渲染器里都保持聚焦。
        assert!(
            fixture
                .mode
                .test_switch_tui_mode(RendererTuiMode::Fullscreen, false)
        );
        assert_eq!(
            fixture.mode.test_ui_focus_identity(),
            Some(focused),
            "the same component must remain focused after switching to fullscreen"
        );
        assert!(
            fixture
                .mode
                .test_switch_tui_mode(RendererTuiMode::Regular, false)
        );
        assert_eq!(
            fixture.mode.test_ui_focus_identity(),
            Some(focused),
            "the same component must remain focused after switching to regular"
        );

        // 输入仍然到达选择器：Esc 取消并关闭它（若焦点指向脱离挂载树的旧目标，输入会被丢弃）。
        fixture.mode.test_ui_handle_input("\x1b");
        common::yield_tasks(20).await;
        assert!(
            !fixture.mode.test_has_active_selector(),
            "the focused selector must receive the Esc key after the mode switch"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn switch_tui_mode_does_not_carry_a_layout_root_focus_target() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        assert!(
            fixture
                .mode
                .test_switch_tui_mode(RendererTuiMode::Fullscreen, false)
        );

        // 全屏渲染器的焦点目标可以是 `LayoutRoot { path }`（布局根不是主屏挂载树的一部分）。
        fixture
            .mode
            .test_set_ui_focus(Some(kiana_tui::tui::FocusTarget::LayoutRoot {
                path: kiana_tui::tui::FocusPath::root(),
            }));
        assert!(matches!(
            fixture.mode.test_ui_focus(),
            Some(kiana_tui::tui::FocusTarget::LayoutRoot { .. })
        ));

        // 切回主屏后没有布局根：直接沿用该目标会让 `handleTerminalInput` 静默丢弃所有输入
        // （`with_component_mut_of(LayoutRoot)` 在 `layout_root == None` 时什么都不做）。
        // TS 的 `setFocus(getFocusedComponent())` 传的是组件引用，不会出现悬垂位置。
        assert!(
            fixture
                .mode
                .test_switch_tui_mode(RendererTuiMode::Regular, false)
        );
        assert!(
            !matches!(
                fixture.mode.test_ui_focus(),
                Some(kiana_tui::tui::FocusTarget::LayoutRoot { .. })
            ),
            "a LayoutRoot focus target must not be carried into the main screen"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ctrl_c_clears_editor_then_second_press_shuts_down() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_set_editor_text("draft");
        let exits: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&exits);
        set_exit_hook_for_test(Some(Box::new(move |code| {
            recorded.borrow_mut().push(code);
        })));

        fixture.mode.test_handle_ctrl_c();
        assert_eq!(fixture.mode.test_editor_text(), "");
        assert!(exits.borrow().is_empty());

        fixture.mode.test_handle_ctrl_c();
        common::wait_until(|| !exits.borrow().is_empty()).await;
        assert_eq!(&*exits.borrow(), &[0]);
        set_exit_hook_for_test(None);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ctrl_d_shuts_down() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        let exits: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&exits);
        set_exit_hook_for_test(Some(Box::new(move |code| {
            recorded.borrow_mut().push(code);
        })));
        fixture.mode.test_handle_ctrl_d();
        common::wait_until(|| !exits.borrow().is_empty()).await;
        assert_eq!(&*exits.borrow(), &[0]);
        set_exit_hook_for_test(None);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn startup_submit_restores_text_and_reports_progress() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_handle_startup_submit("hello".to_string());
        assert_eq!(fixture.mode.test_editor_text(), "hello");
        let rendered = fixture.mode.chat_render_text();
        assert!(
            rendered.contains("Startup is still in progress"),
            "{rendered}"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// 剪贴板（`clipboard-image-native-errors.test.ts` / `clipboard-paste-file-paths.test.ts`）
// ---------------------------------------------------------------------------

struct FakeClipboard {
    file_paths: Mutex<Result<Option<Vec<String>>, String>>,
    image: Mutex<Result<Option<ClipboardImage>, String>>,
    text: Mutex<Result<Option<String>, String>>,
    calls: Mutex<Vec<&'static str>>,
}

impl FakeClipboard {
    fn new(
        file_paths: Result<Option<Vec<String>>, String>,
        image: Result<Option<ClipboardImage>, String>,
        text: Result<Option<String>, String>,
    ) -> Self {
        FakeClipboard {
            file_paths: Mutex::new(file_paths),
            image: Mutex::new(image),
            text: Mutex::new(text),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

#[async_trait::async_trait(?Send)]
impl ClipboardSource for FakeClipboard {
    async fn read_file_paths(&self) -> Result<Option<Vec<String>>, String> {
        self.calls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push("file_paths");
        self.file_paths
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    async fn read_image(&self) -> Result<Option<ClipboardImage>, String> {
        self.calls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push("image");
        self.image
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    async fn read_text(&self) -> Result<Option<String>, String> {
        self.calls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push("text");
        self.text
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

fn temp_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("temp dir")
}

#[tokio::test]
async fn finder_file_paths_take_precedence_over_the_icon_image() {
    let dir = temp_dir();
    let clipboard = FakeClipboard::new(
        Ok(Some(vec![
            "/tmp/screenshot.png".to_string(),
            "/tmp/My Photos/photo.png".to_string(),
        ])),
        Ok(Some(ClipboardImage {
            bytes: vec![0x89, 0x50],
            mime_type: "image/png".to_string(),
        })),
        Ok(None),
    );
    let text = clipboard_paste_text(&clipboard, "", (0, 0), false, dir.path())
        .await
        .expect("paste");
    assert_eq!(
        text.as_deref(),
        Some("/tmp/screenshot.png\n/tmp/My Photos/photo.png")
    );
    assert_eq!(clipboard.calls(), vec!["file_paths"]);
}

#[tokio::test]
async fn clipboard_file_paths_with_control_characters_are_rejected() {
    let dir = temp_dir();
    let clipboard = FakeClipboard::new(
        Ok(Some(vec!["/tmp/photo\x1b]0;unsafe\x07.png".to_string()])),
        Ok(None),
        Ok(Some("ignored".to_string())),
    );
    let error = clipboard_paste_text(&clipboard, "", (0, 0), false, dir.path())
        .await
        .expect_err("control characters");
    assert_eq!(error, "Clipboard file path contains control characters");
    assert_eq!(clipboard.calls(), vec!["file_paths"]);
}

#[tokio::test]
async fn native_file_path_errors_do_not_fall_through_to_the_image() {
    let dir = temp_dir();
    let clipboard = FakeClipboard::new(
        Err("Native clipboard file read failed".to_string()),
        Ok(Some(ClipboardImage {
            bytes: vec![1],
            mime_type: "image/png".to_string(),
        })),
        Ok(Some("text".to_string())),
    );
    let error = clipboard_paste_text(&clipboard, "", (0, 0), false, dir.path())
        .await
        .expect_err("native error");
    assert_eq!(error, "Native clipboard file read failed");
    assert_eq!(clipboard.calls(), vec!["file_paths"]);
}

#[tokio::test]
async fn native_image_errors_abort_without_reading_text() {
    let dir = temp_dir();
    let clipboard = FakeClipboard::new(
        Ok(None),
        Err("Native clipboard operation failed".to_string()),
        Ok(Some("text".to_string())),
    );
    let error = clipboard_paste_text(&clipboard, "", (0, 0), false, dir.path())
        .await
        .expect_err("native image error");
    assert_eq!(error, "Native clipboard operation failed");
    assert_eq!(clipboard.calls(), vec!["file_paths", "image"]);
}

#[tokio::test]
async fn image_paste_writes_a_temporary_file() {
    let dir = temp_dir();
    let clipboard = FakeClipboard::new(
        Ok(None),
        Ok(Some(ClipboardImage {
            bytes: vec![0x89, 0x50, 0x4e, 0x47],
            mime_type: "image/png".to_string(),
        })),
        Ok(Some("text".to_string())),
    );
    let text = clipboard_paste_text(&clipboard, "", (0, 0), false, dir.path())
        .await
        .expect("image paste")
        .expect("path");
    assert!(
        text.starts_with(&dir.path().to_string_lossy().into_owned()),
        "{text}"
    );
    assert!(text.ends_with(".png"), "{text}");
    assert!(std::path::Path::new(&text).exists());
    assert_eq!(clipboard.calls(), vec!["file_paths", "image"]);
}

#[tokio::test]
async fn file_paths_are_separated_from_preceding_punctuation_and_unicode() {
    let dir = temp_dir();
    for (editor_text, cursor_col) in [("Review:", 7usize), ("確認", 2usize)] {
        let clipboard = FakeClipboard::new(
            Ok(Some(vec!["/tmp/photo.png".to_string()])),
            Ok(None),
            Ok(None),
        );
        let text =
            clipboard_paste_text(&clipboard, editor_text, (0, cursor_col), false, dir.path())
                .await
                .expect("paste");
        assert_eq!(text.as_deref(), Some(" /tmp/photo.png"));
    }
}

#[tokio::test]
async fn bash_mode_shell_quotes_file_paths() {
    let dir = temp_dir();
    let clipboard = FakeClipboard::new(
        Ok(Some(vec![
            "/tmp/My Photos/photo.png".to_string(),
            "/tmp/$(touch hacked).png".to_string(),
            "/tmp/plain.png".to_string(),
        ])),
        Ok(None),
        Ok(None),
    );
    let text = clipboard_paste_text(&clipboard, "catDEST", (0, 3), true, dir.path())
        .await
        .expect("paste")
        .expect("text");
    assert_eq!(
        text,
        " '/tmp/My Photos/photo.png' '/tmp/$(touch hacked).png' /tmp/plain.png "
    );
    assert_eq!(clipboard.calls(), vec!["file_paths"]);
}

// ---------------------------------------------------------------------------
// 5724：SIGTERM 走优雅关闭并以 0 退出（子进程观察真实信号）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn sigterm_child_process() {
    if std::env::var("KIANA_SIGTERM_CHILD").ok().as_deref() != Some("1") {
        return;
    }
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        if let Ok(ready) = std::env::var("KIANA_SIGTERM_READY") {
            std::fs::write(ready, "ready").expect("ready marker");
        }
        std::future::pending::<()>().await;
    })
    .await;
}

#[test]
fn sigterm_exits_with_zero_not_signal_death() {
    let exe = std::env::current_exe().expect("current exe");
    let dir = tempfile::tempdir().expect("temp dir");
    let ready = dir.path().join("ready");
    let mut child = std::process::Command::new(exe)
        .args(["--exact", "sigterm_child_process", "--nocapture"])
        .env("KIANA_SIGTERM_CHILD", "1")
        .env("KIANA_SIGTERM_READY", ready.to_string_lossy().into_owned())
        .env("KIANA_OFFLINE", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn child");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !ready.exists() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("child did not become ready");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    #[cfg(unix)]
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => {
                assert_eq!(status.code(), Some(0), "status={status:?}");
                break;
            }
            None => {
                assert!(std::time::Instant::now() < deadline, "child did not exit");
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn queued_startup_input_is_returned_before_installing_a_callback() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_queue_user_input("queued prompt");
        assert_eq!(fixture.mode.test_get_user_input().await, "queued prompt");
        assert_eq!(fixture.mode.test_pending_user_input_count(), 0);
    })
    .await;
}

// ---------------------------------------------------------------------------
// `interactive-mode-suspend.test.ts`（Ctrl+Z 的平台注入）
// ---------------------------------------------------------------------------

/// SIGCONT 处理器槽位（测试用来手动触发“恢复”）。
type SigcontSlot = Rc<RefCell<Option<Box<dyn FnMut()>>>>;

struct RecordingCtrlZ {
    windows: bool,
    calls: Rc<RefCell<Vec<String>>>,
    sigcont: SigcontSlot,
    kill_error: Option<&'static str>,
}

impl RecordingCtrlZ {
    fn new(windows: bool) -> Self {
        RecordingCtrlZ {
            windows,
            calls: Rc::new(RefCell::new(Vec::new())),
            sigcont: Rc::new(RefCell::new(None)),
            kill_error: None,
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl CtrlZPlatform for RecordingCtrlZ {
    fn is_windows(&self) -> bool {
        self.windows
    }

    fn start_keep_alive(&mut self) -> Box<dyn FnMut()> {
        self.calls.borrow_mut().push("setInterval".to_string());
        let calls = Rc::clone(&self.calls);
        Box::new(move || calls.borrow_mut().push("clearInterval".to_string()))
    }

    fn ignore_sigint(&mut self) -> Box<dyn FnMut()> {
        self.calls.borrow_mut().push("on:SIGINT".to_string());
        let calls = Rc::clone(&self.calls);
        Box::new(move || calls.borrow_mut().push("removeListener:SIGINT".to_string()))
    }

    fn on_sigcont(&mut self, handler: Box<dyn FnMut() + 'static>) {
        self.calls.borrow_mut().push("once:SIGCONT".to_string());
        *self.sigcont.borrow_mut() = Some(handler);
    }

    fn kill_sigtstp(&mut self) -> std::io::Result<()> {
        self.calls.borrow_mut().push("kill".to_string());
        match self.kill_error {
            Some(message) => Err(std::io::Error::other(message)),
            None => Ok(()),
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn ctrl_z_shows_the_windows_message_and_skips_suspend() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        fixture.output.borrow_mut().clear();
        let mut platform = RecordingCtrlZ::new(true);
        fixture.mode.test_handle_ctrl_z_with_platform(&mut platform);
        assert!(platform.calls().is_empty(), "{:?}", platform.calls());
        assert!(
            fixture
                .mode
                .chat_render_text()
                .contains("Suspend to background is not supported on Windows")
        );
        assert!(!fixture.output.borrow().contains("\x1b[?2004l"));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ctrl_z_suspends_and_restores_the_tui_on_sigcont() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        fixture.output.borrow_mut().clear();
        let mut platform = RecordingCtrlZ::new(false);
        fixture.mode.test_handle_ctrl_z_with_platform(&mut platform);

        assert_eq!(
            platform.calls(),
            vec!["setInterval", "on:SIGINT", "once:SIGCONT", "kill"]
        );
        assert!(
            fixture.output.borrow().contains("\x1b[?2004l"),
            "the TUI must be stopped before SIGTSTP"
        );

        let mut handler = platform
            .sigcont
            .borrow_mut()
            .take()
            .expect("SIGCONT handler installed");
        handler();
        let calls = platform.calls();
        assert!(calls.contains(&"clearInterval".to_string()), "{calls:?}");
        assert!(
            calls.contains(&"removeListener:SIGINT".to_string()),
            "{calls:?}"
        );
        assert_eq!(
            fixture.output.borrow().matches("\x1b[?2004h").count(),
            1,
            "the TUI must be started again: {:?}",
            fixture.output.borrow()
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ctrl_z_cleans_up_temporary_handlers_when_suspension_fails() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        fixture.output.borrow_mut().clear();
        let mut platform = RecordingCtrlZ::new(false);
        platform.kill_error = Some("suspend failed");

        let result = kiana_interactive::components::markdown_transform::catch_unwind_guarded(
            std::panic::AssertUnwindSafe(|| {
                fixture.mode.test_handle_ctrl_z_with_platform(&mut platform)
            }),
        );
        assert!(result.is_err(), "suspension failure must propagate");
        let calls = platform.calls();
        assert!(calls.contains(&"setInterval".to_string()), "{calls:?}");
        assert!(calls.contains(&"clearInterval".to_string()), "{calls:?}");
        assert!(
            calls.contains(&"removeListener:SIGINT".to_string()),
            "{calls:?}"
        );
        assert_eq!(
            fixture.output.borrow().matches("\x1b[?2004h").count(),
            0,
            "the TUI must not be restarted"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// `5080-signal-shutdown-extension-cleanup.test.ts`
// ---------------------------------------------------------------------------

fn shutdown_logging_factory(
    log: Rc<RefCell<Vec<String>>>,
    output: Rc<RefCell<String>>,
) -> ExtensionFactory {
    Arc::new(move |kiana| {
        let log = Rc::clone(&log);
        let output = Rc::clone(&output);
        let _ = kiana.on(
            kiana_ext_api::events::ExtensionEventKind::SessionShutdown,
            kiana_ext_api::events::ExtensionHandler::Standard(Arc::new(move |event, _ctx| {
                if matches!(
                    event,
                    kiana_ext_api::events::ExtensionEvent::Session(
                        kiana_ext_api::events::SessionEvent::Shutdown(_)
                    )
                ) {
                    log.borrow_mut().push(format!(
                        "shutdown:stop_written={}",
                        output.borrow().contains("\x1b[?2004l")
                    ));
                }
                Box::pin(async { Ok::<_, Box<dyn std::error::Error + Send + Sync>>(None) })
            })),
        );
        Box::pin(async {})
    })
}

#[tokio::test(flavor = "current_thread")]
async fn signal_shutdown_emits_session_shutdown_before_terminal_writes() {
    let _theme = theme();
    common::run_local(async {
        let log = Rc::new(RefCell::new(Vec::new()));
        let log_for_factory = Rc::clone(&log);
        let mut fixture =
            fixture_with(move |output| Some(shutdown_logging_factory(log_for_factory, output)))
                .await;
        fixture.mode.init().await;
        fixture.output.borrow_mut().clear();
        let exits: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&exits);
        set_exit_hook_for_test(Some(Box::new(move |code| {
            recorded.borrow_mut().push(code);
        })));

        fixture.mode.test_shutdown(true).await;

        assert_eq!(&*exits.borrow(), &[0]);
        assert_eq!(&*log.borrow(), &["shutdown:stop_written=false".to_string()]);
        set_exit_hook_for_test(None);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn interactive_quit_stops_the_tui_before_session_shutdown() {
    let _theme = theme();
    common::run_local(async {
        let log = Rc::new(RefCell::new(Vec::new()));
        let log_for_factory = Rc::clone(&log);
        let mut fixture =
            fixture_with(move |output| Some(shutdown_logging_factory(log_for_factory, output)))
                .await;
        fixture.mode.init().await;
        fixture.output.borrow_mut().clear();
        let exits: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&exits);
        set_exit_hook_for_test(Some(Box::new(move |code| {
            recorded.borrow_mut().push(code);
        })));

        fixture.mode.test_shutdown(false).await;

        assert_eq!(&*exits.borrow(), &[0]);
        assert_eq!(&*log.borrow(), &["shutdown:stop_written=true".to_string()]);
        set_exit_hook_for_test(None);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn interactive_quit_prints_the_resume_hint_only_for_persisted_sessions() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        // 让会话文件真实存在（`formatResumeCommand` 要求文件存在）。
        if let Some(session_file) = fixture.mode.session().session_file() {
            std::fs::write(&session_file, "\n").expect("session file");
        }
        let stdout: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&stdout);
        set_stdout_hook_for_test(Some(Box::new(move |text| {
            recorded.borrow_mut().push(text.to_string());
        })));
        kiana_interactive::interactive_mode::lifecycle::set_stdout_is_tty_for_test(Some(true));
        let exits: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
        let recorded_exits = Rc::clone(&exits);
        set_exit_hook_for_test(Some(Box::new(move |code| {
            recorded_exits.borrow_mut().push(code);
        })));

        fixture.mode.test_shutdown(false).await;

        let printed = stdout.borrow().join("");
        assert!(
            printed.contains("To resume this session:") && printed.contains("--session"),
            "{printed:?}"
        );
        set_exit_hook_for_test(None);
        set_stdout_hook_for_test(None);
        kiana_interactive::interactive_mode::lifecycle::set_stdout_is_tty_for_test(None);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn signal_shutdown_does_not_print_the_resume_hint() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        if let Some(session_file) = fixture.mode.session().session_file() {
            std::fs::write(&session_file, "\n").expect("session file");
        }
        let stdout: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&stdout);
        set_stdout_hook_for_test(Some(Box::new(move |text| {
            recorded.borrow_mut().push(text.to_string());
        })));
        kiana_interactive::interactive_mode::lifecycle::set_stdout_is_tty_for_test(Some(true));
        let exits: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
        let recorded_exits = Rc::clone(&exits);
        set_exit_hook_for_test(Some(Box::new(move |code| {
            recorded_exits.borrow_mut().push(code);
        })));

        fixture.mode.test_shutdown(true).await;

        let printed = stdout.borrow().join("");
        assert!(!printed.contains("To resume this session:"), "{printed:?}");
        set_exit_hook_for_test(None);
        set_stdout_hook_for_test(None);
        kiana_interactive::interactive_mode::lifecycle::set_stdout_is_tty_for_test(None);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn dead_terminal_errors_exit_129_without_terminal_writes_or_crash_record() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        fixture.output.borrow_mut().clear();

        let code = fixture
            .mode
            .test_uncaught_crash_code(&std::io::Error::from_raw_os_error(libc::EIO));
        assert_eq!(code, 129);
        assert!(
            !fixture.output.borrow().contains("\x1b[?2004l"),
            "dead-terminal exit must not write restore sequences"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn other_uncaught_errors_are_recorded_as_crashes() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        let agent_dir = tempfile::tempdir().expect("agent dir");
        let previous = std::env::var_os("KIANA_CODING_AGENT_DIR");
        unsafe { std::env::set_var("KIANA_CODING_AGENT_DIR", agent_dir.path()) };

        let error = std::io::Error::other("boom");
        let code = fixture.mode.test_uncaught_crash_code(&error);

        let crash_path = agent_dir.path().join("crashes.json");
        let records = kiana_resources::crash_log::read_crash_log(&crash_path.to_string_lossy());
        unsafe {
            match previous {
                Some(value) => std::env::set_var("KIANA_CODING_AGENT_DIR", value),
                None => std::env::remove_var("KIANA_CODING_AGENT_DIR"),
            }
        }
        assert_eq!(code, 1);
        assert!(
            records.iter().any(|record| record
                .message()
                .is_some_and(|message| message.contains("boom"))),
            "{records:?}"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn bug_command_rejections_follow_the_crash_path() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        let agent_dir = tempfile::tempdir().expect("agent dir");
        let previous = std::env::var_os("KIANA_CODING_AGENT_DIR");
        unsafe { std::env::set_var("KIANA_CODING_AGENT_DIR", agent_dir.path()) };
        let exits: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&exits);
        set_exit_hook_for_test(Some(Box::new(move |code| {
            recorded.borrow_mut().push(code);
        })));

        // `reportBug` 的拒绝（未捕获的 `await`）→ `uncaughtCrash`。
        fixture.mode.test_finish_bug_command(Err(
            kiana_session_core::session_manager::SessionManagerError::InvalidSessionId,
        ));

        unsafe {
            match previous {
                Some(value) => std::env::set_var("KIANA_CODING_AGENT_DIR", value),
                None => std::env::remove_var("KIANA_CODING_AGENT_DIR"),
            }
        }
        set_exit_hook_for_test(None);
        assert_eq!(&*exits.borrow(), &[1]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn external_editor_io_failures_follow_the_crash_path() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        let agent_dir = tempfile::tempdir().expect("agent dir");
        let previous = std::env::var_os("KIANA_CODING_AGENT_DIR");
        unsafe { std::env::set_var("KIANA_CODING_AGENT_DIR", agent_dir.path()) };
        let exits: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&exits);
        set_exit_hook_for_test(Some(Box::new(move |code| {
            recorded.borrow_mut().push(code);
        })));

        // `mkdtempSync`/`writeFileSync`/`readFileSync` 失败 → Promise 拒绝。
        fixture
            .mode
            .test_finish_open_external_editor(Err(std::io::Error::other(
                "external editor io failure",
            )));

        unsafe {
            match previous {
                Some(value) => std::env::set_var("KIANA_CODING_AGENT_DIR", value),
                None => std::env::remove_var("KIANA_CODING_AGENT_DIR"),
            }
        }
        set_exit_hook_for_test(None);
        assert_eq!(&*exits.borrow(), &[1]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn unregister_removes_all_signal_cleanup_handlers() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        assert!(fixture.mode.test_signal_cleanup_handler_count() > 0);
        fixture.mode.test_unregister_signal_handlers();
        assert_eq!(fixture.mode.test_signal_cleanup_handler_count(), 0);
    })
    .await;
}

// ---------------------------------------------------------------------------
// `8611-thinking-toggle-pending-bash-output.test.ts`
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn thinking_toggle_preserves_pending_tool_output() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        fixture
            .mode
            .test_handle_event(
                &kiana_session::agent_session::AgentSessionEvent::ToolExecutionStart {
                    tool_call_id: "tool-8611".to_string(),
                    tool_name: "bash".to_string(),
                    args: serde_json::json!({"command": "echo first; sleep 10"}),
                    parent_tool_call_id: None,
                },
            )
            .await;
        fixture
            .mode
            .test_handle_event(
                &kiana_session::agent_session::AgentSessionEvent::ToolExecutionUpdate {
                    tool_call_id: "tool-8611".to_string(),
                    tool_name: "bash".to_string(),
                    args: serde_json::json!({"command": "echo first; sleep 10"}),
                    partial_result: serde_json::json!({
                        "content": [{"type": "text", "text": "first"}]
                    }),
                    parent_tool_call_id: None,
                },
            )
            .await;
        assert!(fixture.mode.chat_render_text().contains("first"));

        fixture.mode.test_toggle_thinking_block_visibility();

        assert!(fixture.mode.settings_manager().get_hide_thinking_block());
        let rendered = fixture.mode.chat_render_text();
        assert!(rendered.contains("first"), "{rendered}");
        assert!(fixture.mode.test_pending_tool_count() == 1);
    })
    .await;
}

/// #5724：信号触发的清理期间必须保持信号处理器注册（signal-exit 会在同一 dispatch 里重发）。
#[tokio::test(flavor = "current_thread")]
async fn signal_shutdown_keeps_signal_handlers_registered_while_cleanup_runs() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        let before = fixture.mode.test_signal_cleanup_handler_count();
        assert!(before > 0);
        let exits: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&exits);
        set_exit_hook_for_test(Some(Box::new(move |code| {
            recorded.borrow_mut().push(code);
        })));

        fixture.mode.test_shutdown(true).await;

        assert_eq!(&*exits.borrow(), &[0]);
        assert_eq!(
            fixture.mode.test_signal_cleanup_handler_count(),
            before,
            "signal handlers must stay registered until the process exits"
        );
        set_exit_hook_for_test(None);
    })
    .await;
}

// ---------------------------------------------------------------------------
// uncaughtException：未捕获 panic 必须记录崩溃并以 1 退出（Node 的 uncaughtException 路径）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn uncaught_panic_child_process() {
    if std::env::var("KIANA_PANIC_CHILD").ok().as_deref() != Some("1") {
        return;
    }
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        if let Ok(ready) = std::env::var("KIANA_PANIC_READY") {
            std::fs::write(ready, "ready").expect("ready marker");
        }
        panic!("boom from an uncaught panic");
    })
    .await;
}

#[test]
fn uncaught_panic_exits_with_one_and_records_a_crash() {
    let exe = std::env::current_exe().expect("current exe");
    let dir = tempfile::tempdir().expect("temp dir");
    let ready = dir.path().join("ready");
    let agent_dir = dir.path().join("agent");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    let mut child = std::process::Command::new(exe)
        .args(["--exact", "uncaught_panic_child_process", "--nocapture"])
        .env("KIANA_PANIC_CHILD", "1")
        .env("KIANA_PANIC_READY", ready.to_string_lossy().into_owned())
        .env(
            "KIANA_CODING_AGENT_DIR",
            agent_dir.to_string_lossy().into_owned(),
        )
        .env("KIANA_OFFLINE", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn child");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !ready.exists() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("child did not become ready");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => break status,
            None => {
                assert!(std::time::Instant::now() < deadline, "child did not exit");
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
    };
    assert_eq!(status.code(), Some(1), "status={status:?}");

    let records = kiana_resources::crash_log::read_crash_log(
        &agent_dir.join("crashes.json").to_string_lossy(),
    );
    let record = records
        .iter()
        .find(|record| record.kind() == Some("uncaught_exception"))
        .unwrap_or_else(|| panic!("no uncaught_exception record: {records:?}"));
    assert!(
        record
            .message()
            .is_some_and(|message| message.contains("boom from an uncaught panic")),
        "{record:?}"
    );
    let stack = record
        .as_map()
        .get("stack")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert!(
        stack.starts_with("Error: boom from an uncaught panic"),
        "crash stack must carry the panic message: {stack}"
    );
    assert!(
        stack.contains("uncaught_panic_child_process"),
        "crash stack must carry the panic location: {stack}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn fatal_runtime_error_child_process() {
    if std::env::var("KIANA_FATAL_CHILD").ok().as_deref() != Some("1") {
        return;
    }
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        if let Ok(ready) = std::env::var("KIANA_FATAL_READY") {
            std::fs::write(ready, "ready").expect("ready marker");
        }
        fixture
            .mode
            .test_handle_fatal_runtime_error("Failed to create session", "fatal boom")
            .await;
    })
    .await;
}

#[test]
fn fatal_runtime_error_records_a_stack_and_exits_with_one() {
    let exe = std::env::current_exe().expect("current exe");
    let dir = tempfile::tempdir().expect("temp dir");
    let ready = dir.path().join("ready");
    let agent_dir = dir.path().join("agent");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    let mut child = std::process::Command::new(exe)
        .args([
            "--exact",
            "fatal_runtime_error_child_process",
            "--nocapture",
        ])
        .env("KIANA_FATAL_CHILD", "1")
        .env("KIANA_FATAL_READY", ready.to_string_lossy().into_owned())
        .env(
            "KIANA_CODING_AGENT_DIR",
            agent_dir.to_string_lossy().into_owned(),
        )
        .env("KIANA_OFFLINE", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn child");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !ready.exists() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("child did not become ready");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => break status,
            None => {
                assert!(std::time::Instant::now() < deadline, "child did not exit");
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
    };
    assert_eq!(status.code(), Some(1), "status={status:?}");

    let records = kiana_resources::crash_log::read_crash_log(
        &agent_dir.join("crashes.json").to_string_lossy(),
    );
    let record = records
        .iter()
        .find(|record| record.kind() == Some("fatal_error"))
        .unwrap_or_else(|| panic!("no fatal_error record: {records:?}"));
    assert!(
        record
            .message()
            .is_some_and(|message| message.contains("fatal boom")),
        "{record:?}"
    );
    let stack = record
        .as_map()
        .get("stack")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert!(
        stack.starts_with("Error: fatal boom"),
        "fatal crash record must carry a stack: {stack}"
    );
}
