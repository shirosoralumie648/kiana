//! `test/interactive-mode-extension-dialogs.test.ts` 的移植（4 条）。

#![allow(
    clippy::await_holding_refcell_ref,
    clippy::await_holding_lock,
    reason = "单线程 LocalSet 测试：扩展工厂没有 Send/Sync 约束，且不存在并发借用者"
)]

mod common;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::{Mutex, MutexGuard};

use kiana_ext_api::LocalBoxFuture;
use kiana_ext_api::ui::{
    CustomDoneFn, CustomFactory, CustomOptions, ExtensionUIDialogOptions, ExtensionUiContext,
};
use kiana_tui::components::text_component::Text;
use kiana_tui::tui::Component;
use tokio_util::sync::CancellationToken;

use kiana_config::settings::TuiMode;
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

async fn fullscreen_fixture() -> Fixture {
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
            tui_mode: Some(TuiMode::Fullscreen),
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

#[tokio::test(flavor = "current_thread")]
async fn shows_the_earlier_confirm_again_after_the_later_one_is_answered() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        let ui = context(&fixture.mode);

        let ui_one = Arc::clone(&ui);
        let first =
            tokio::task::spawn_local(async move { ui_one.confirm("First?", "one", None).await });
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 1).await;

        let ui_two = Arc::clone(&ui);
        let second =
            tokio::task::spawn_local(async move { ui_two.confirm("Second?", "two", None).await });
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 2).await;
        let top = fixture.mode.borrow().test_dialog_render(1);
        assert!(top.contains("Second?"), "{top}");

        // 栈顶选 Yes（默认选中第一项）。
        fixture.mode.borrow_mut().test_send_to_top_dialog("\r");
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 1).await;
        assert!(second.await.expect("second dialog result"));
        let top = fixture.mode.borrow().test_dialog_render(0);
        assert!(top.contains("First?"), "{top}");

        fixture.mode.borrow_mut().test_send_to_top_dialog("\r");
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 0).await;
        assert!(first.await.expect("first dialog result"));
        assert_eq!(fixture.mode.borrow().test_editor_slot_child_count(), 1);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn removes_a_hidden_dialog_when_its_signal_aborts() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        let ui = context(&fixture.mode);
        let signal = CancellationToken::new();

        let ui_one = Arc::clone(&ui);
        let signal_one = signal.clone();
        let first = tokio::task::spawn_local(async move {
            let options = ExtensionUIDialogOptions {
                signal: Some(signal_one),
                timeout: None,
            };
            ui_one.confirm("First?", "one", Some(options)).await
        });
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 1).await;

        let ui_two = Arc::clone(&ui);
        let second =
            tokio::task::spawn_local(async move { ui_two.confirm("Second?", "two", None).await });
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 2).await;

        signal.cancel();
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 1).await;
        assert!(!first.await.expect("aborted dialog"));
        // 栈顶仍是第二个对话框。
        let top = fixture.mode.borrow().test_dialog_render(0);
        assert!(top.contains("Second?"), "{top}");
        fixture.mode.borrow_mut().test_send_to_top_dialog("\r");
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 0).await;
        assert!(second.await.expect("second dialog"));
    })
    .await;
}

fn simple_custom_factory(done_holder: Arc<Mutex<Option<CustomDoneFn>>>) -> CustomFactory {
    Arc::new(move |_tui, _theme, _keybindings, done| {
        *done_holder
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(done);
        Box::pin(async {
            Box::new(Text::with_padding("custom component", 1, 0)) as Box<dyn Component>
        })
    })
}

#[tokio::test(flavor = "current_thread")]
async fn stacks_concurrent_inline_custom_components() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        let ui = context(&fixture.mode);
        let done_one: Arc<Mutex<Option<CustomDoneFn>>> = Arc::new(Mutex::new(None));
        let done_two: Arc<Mutex<Option<CustomDoneFn>>> = Arc::new(Mutex::new(None));

        let ui_one = Arc::clone(&ui);
        let holder_one = Arc::clone(&done_one);
        let first = tokio::task::spawn_local(async move {
            let factory = simple_custom_factory(holder_one);
            ui_one
                .custom(
                    factory,
                    Some(CustomOptions {
                        overlay: Some(false),
                        overlay_options: None,
                        on_handle: None,
                    }),
                )
                .await
        });
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 1).await;

        let ui_two = Arc::clone(&ui);
        let holder_two = Arc::clone(&done_two);
        let second = tokio::task::spawn_local(async move {
            let factory = simple_custom_factory(holder_two);
            ui_two
                .custom(
                    factory,
                    Some(CustomOptions {
                        overlay: Some(false),
                        overlay_options: None,
                        on_handle: None,
                    }),
                )
                .await
        });
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 2).await;

        // 先结束顶层。
        {
            let guard = done_two.lock().unwrap_or_else(|error| error.into_inner());
            guard.as_ref().expect("second done")(Box::new(String::from("two")));
        }
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 1).await;
        assert_eq!(
            second
                .await
                .expect("second custom")
                .downcast::<String>()
                .expect("string result")
                .as_str(),
            "two"
        );
        let top = fixture.mode.borrow().test_dialog_render(0);
        assert!(top.contains("custom component"), "{top}");

        {
            let guard = done_one.lock().unwrap_or_else(|error| error.into_inner());
            guard.as_ref().expect("first done")(Box::new(String::from("one")));
        }
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 0).await;
        assert_eq!(
            first
                .await
                .expect("first custom")
                .downcast::<String>()
                .expect("string result")
                .as_str(),
            "one"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn shows_a_confirm_opened_from_an_inline_custom_component_and_then_restores_the_component() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        let ui = context(&fixture.mode);
        let bridge = Arc::clone(&ui);
        let done_holder: Arc<Mutex<Option<CustomDoneFn>>> = Arc::new(Mutex::new(None));
        let holder = Arc::clone(&done_holder);

        let ui_for_custom = Arc::clone(&ui);
        let custom = tokio::task::spawn_local(async move {
            let factory: CustomFactory = Arc::new(move |_tui, _theme, _keybindings, done| {
                *holder.lock().unwrap_or_else(|error| error.into_inner()) = Some(done);
                Box::pin(async {
                    Box::new(Text::with_padding("inline custom", 1, 0)) as Box<dyn Component>
                })
            });
            ui_for_custom
                .custom(
                    factory,
                    Some(CustomOptions {
                        overlay: Some(false),
                        overlay_options: None,
                        on_handle: None,
                    }),
                )
                .await
        });
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 1).await;

        // 自定义组件内部打开 confirm。
        let nested =
            tokio::task::spawn_local(
                async move { bridge.confirm("Nested?", "inside", None).await },
            );
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 2).await;
        fixture.mode.borrow_mut().test_send_to_top_dialog("\r");
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 1).await;
        assert!(nested.await.expect("nested confirm"));
        let top = fixture.mode.borrow().test_dialog_render(0);
        assert!(top.contains("inline custom"), "{top}");

        {
            let guard = done_holder
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            guard.as_ref().expect("custom done")(Box::new(()));
        }
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 0).await;
        let _ = custom.await.expect("custom result");
    })
    .await;
}

/// 保留 `LocalBoxFuture` 的导入意图（自定义工厂的返回类型别名）。
const _: fn() = || {
    let _ = std::mem::size_of::<LocalBoxFuture<'static, ()>>();
};

#[tokio::test(flavor = "current_thread")]
async fn sync_getters_read_the_published_snapshot_and_notify_renders() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        let ui = context(&fixture.mode);
        ui.set_editor_text("hello");
        assert_eq!(ui.get_editor_text(), "hello");
        assert!(!ui.get_tools_expanded());
        ui.set_tools_expanded(true);
        assert!(ui.get_tools_expanded());
        assert!(ui.get_editor_component().is_none());
        ui.notify("extension notice", None);
        common::wait_until(|| {
            fixture
                .mode
                .borrow()
                .chat_render_text()
                .contains("extension notice")
        })
        .await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn set_theme_persists_valid_names_and_ignores_invalid_ones() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        let ui = context(&fixture.mode);
        let result = ui.set_theme(kiana_ext_api::ui::ThemeOrName::Name("light".to_string()));
        assert!(result.success, "{result:?}");
        common::wait_until(|| {
            fixture
                .mode
                .borrow()
                .settings_manager()
                .get_theme()
                .as_deref()
                == Some("light")
        })
        .await;

        let before = fixture.mode.borrow().settings_manager().get_theme();
        let result = ui.set_theme(kiana_ext_api::ui::ThemeOrName::Name(
            "definitely-not-a-theme".to_string(),
        ));
        assert!(!result.success, "{result:?}");
        assert!(result.error.is_some());
        assert_eq!(fixture.mode.borrow().settings_manager().get_theme(), before);

        // 恢复 dark，避免影响同进程的其它用例。
        kiana_theme::init_theme(Some("dark"), false);
    })
    .await;
}

// ---------------------------------------------------------------------------
// 审阅修复回归：setToolsExpanded / 状态指示器 / 实时 getter / 活动编辑器路由
// ---------------------------------------------------------------------------

use kiana_ext_api::ui::EditorFactory;
use kiana_tui::components::editor::EditorTheme;
use kiana_tui::editor_component::EditorComponent;
use kiana_tui::tui::Tui;

/// 最小扩展编辑器：记录历史调用，渲染一个固定标签。
struct TestEditor {
    text: String,
    label: &'static str,
    history: Arc<std::sync::Mutex<Vec<String>>>,
}

impl TestEditor {
    fn new(label: &'static str, history: Arc<std::sync::Mutex<Vec<String>>>) -> Self {
        TestEditor {
            text: String::new(),
            label,
            history,
        }
    }
}

impl Component for TestEditor {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec![self.label.to_string()]
    }
    fn invalidate(&mut self) {}
}

impl EditorComponent for TestEditor {
    fn get_text(&self) -> String {
        self.text.clone()
    }
    fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
    }
    fn add_to_history(&mut self, text: &str) {
        self.history
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(text.to_string());
    }
    fn insert_text_at_cursor(&mut self, text: &str) {
        // 观察实际插入的文本（测试编辑器没有真实光标）。
        self.text = text.to_string();
    }
}

fn editor_factory(
    label: &'static str,
    history: Arc<std::sync::Mutex<Vec<String>>>,
) -> EditorFactory {
    Arc::new(
        move |_tui: Arc<dyn Tui>,
              _theme: &EditorTheme,
              _keybindings: Arc<kiana_config::keybindings::KeybindingsManager>| {
            Box::new(TestEditor::new(label, Arc::clone(&history))) as Box<dyn EditorComponent>
        },
    )
}

/// `setToolsExpanded` 必须覆盖已渲染的历史工具行（它们包在 `SharedComponentProxy` 里）。
#[tokio::test(flavor = "current_thread")]
async fn set_tools_expanded_expands_rendered_tool_rows() {
    use kiana_interactive::components::tool_execution::{
        ToolExecutionComponent, ToolExecutionOptions,
    };

    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        let tool = Rc::new(RefCell::new(ToolExecutionComponent::new(
            "slow_tool",
            "tool-expand",
            serde_json::json!({"command": "seq 12"}),
            ToolExecutionOptions::default(),
            None,
            Rc::new(|| {}),
            std::env::temp_dir(),
        )));
        fixture.mode.borrow().test_add_tool_component_to_chat(&tool);
        assert!(!tool.borrow().is_expanded());

        fixture.mode.borrow_mut().test_set_tools_expanded(true);
        assert!(
            tool.borrow().is_expanded(),
            "rendered tool rows must follow setToolsExpanded"
        );

        fixture.mode.borrow_mut().test_set_tools_expanded(false);
        assert!(!tool.borrow().is_expanded());
    })
    .await;
}

/// 换编辑器（扩展 `setEditorComponent` / `resetExtensionUI`）时活动状态指示器不能消失。
#[tokio::test(flavor = "current_thread")]
async fn swapping_editors_keeps_the_active_status_indicator_visible() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        fixture.mode.borrow_mut().init().await;
        fixture
            .mode
            .borrow_mut()
            .test_show_working_status_indicator();
        assert!(
            fixture
                .mode
                .borrow()
                .test_active_working_indicator_embedded(),
            "the default editor must embed the working indicator"
        );

        let ui = context(&fixture.mode);
        let history = Arc::new(std::sync::Mutex::new(Vec::new()));
        ui.set_editor_component(Some(editor_factory("CUSTOM_EDITOR", history)));
        common::wait_until(|| {
            fixture
                .mode
                .borrow()
                .test_editor_slot_text()
                .contains("CUSTOM_EDITOR")
        })
        .await;
        assert!(
            !fixture
                .mode
                .borrow()
                .test_active_working_indicator_embedded(),
            "a custom editor must not embed the default indicator"
        );
        let status = fixture.mode.borrow().test_status_container_text();
        assert!(!status.trim().is_empty(), "status row lost: {status:?}");

        ui.set_editor_component(None);
        common::wait_until(|| {
            fixture
                .mode
                .borrow()
                .test_active_working_indicator_embedded()
        })
        .await;
        assert!(
            fixture
                .mode
                .borrow()
                .test_status_container_text()
                .trim()
                .is_empty(),
            "status row must be cleared when the indicator moves into the editor"
        );
    })
    .await;
}

/// 同步 getter 读取实时状态（用户键入、Ctrl+O 路径、内部 reset），而不是旧快照。
#[tokio::test(flavor = "current_thread")]
async fn sync_getters_read_live_state_not_a_stale_snapshot() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        fixture.mode.borrow_mut().init().await;
        let ui = context(&fixture.mode);

        // 用户键入（不经桥接、也不发布快照）。
        fixture
            .mode
            .borrow_mut()
            .test_set_editor_text("typed by user");
        assert_eq!(ui.get_editor_text(), "typed by user");

        // 键位路径（toggleToolOutputExpansion）不是扩展请求。
        fixture
            .mode
            .borrow_mut()
            .test_toggle_tool_output_expansion();
        assert!(ui.get_tools_expanded());
        fixture
            .mode
            .borrow_mut()
            .test_toggle_tool_output_expansion();
        assert!(!ui.get_tools_expanded());

        // 内部 reset 清除编辑器工厂；快照路径之外必须仍然可见。
        let history = Arc::new(std::sync::Mutex::new(Vec::new()));
        ui.set_editor_component(Some(editor_factory("CUSTOM_EDITOR", Arc::clone(&history))));
        common::wait_until(|| ui.get_editor_component().is_some()).await;
        fixture.mode.borrow_mut().test_reset_extension_ui();
        assert!(ui.get_editor_component().is_none());
    })
    .await;
}

/// 压缩排队 / 历史写入走活动编辑器（扩展编辑器），而不是隐藏的默认编辑器。
#[tokio::test(flavor = "current_thread")]
async fn compaction_queue_writes_history_to_the_active_editor() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        fixture.mode.borrow_mut().init().await;
        let ui = context(&fixture.mode);
        let history = Arc::new(std::sync::Mutex::new(Vec::new()));
        ui.set_editor_component(Some(editor_factory("CUSTOM_EDITOR", Arc::clone(&history))));
        common::wait_until(|| {
            fixture
                .mode
                .borrow()
                .test_editor_slot_text()
                .contains("CUSTOM_EDITOR")
        })
        .await;

        fixture
            .mode
            .borrow_mut()
            .test_queue_compaction_message("queued text", true);
        assert_eq!(
            &*history.lock().unwrap_or_else(|error| error.into_inner()),
            &["queued text".to_string()]
        );
    })
    .await;
}

/// Alt+Enter 在没有提交处理器时保留编辑器文本（TS 只在 onSubmit 存在时清空）。
#[tokio::test(flavor = "current_thread")]
async fn follow_up_keeps_editor_text_when_no_submit_handler_exists() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        // 不调用 init：setupEditorSubmitHandler 尚未安装。
        fixture.mode.borrow_mut().test_set_editor_text("draft");
        fixture.mode.borrow_mut().test_handle_follow_up().await;
        assert_eq!(fixture.mode.borrow().test_editor_text(), "draft");
    })
    .await;
}

/// 自定义编辑器没有 `getCursor()`：粘贴文件路径不加任何前后空格。
#[tokio::test(flavor = "current_thread")]
async fn clipboard_paths_from_a_custom_editor_have_no_cursor_spacing() {
    let _theme = theme();
    common::run_local(async {
        struct FakeClipboard;
        #[async_trait::async_trait(?Send)]
        impl kiana_interactive::interactive_mode::input::ClipboardSource for FakeClipboard {
            async fn read_file_paths(&self) -> Result<Option<Vec<String>>, String> {
                Ok(Some(vec!["/tmp/photo.png".to_string()]))
            }
            async fn read_image(
                &self,
            ) -> Result<Option<kiana_core::image_process::ClipboardImage>, String> {
                Ok(None)
            }
            async fn read_text(&self) -> Result<Option<String>, String> {
                Ok(Some("ignored".to_string()))
            }
        }

        let fixture = fixture().await;
        fixture.mode.borrow_mut().init().await;
        let ui = context(&fixture.mode);
        let history = Arc::new(std::sync::Mutex::new(Vec::new()));
        ui.set_editor_component(Some(editor_factory("CUSTOM_EDITOR", history)));
        common::wait_until(|| {
            fixture
                .mode
                .borrow()
                .test_editor_slot_text()
                .contains("CUSTOM_EDITOR")
        })
        .await;
        fixture.mode.borrow_mut().test_set_editor_text("abc");

        fixture
            .mode
            .borrow_mut()
            .test_handle_clipboard_paste_with(&FakeClipboard)
            .await;
        // `abc` 末尾没有空白，但 TS 在无 cursor 时不加前导空格；旧实现会插到末尾并加空格。
        assert_eq!(fixture.mode.borrow().test_editor_text(), "/tmp/photo.png");
    })
    .await;
}

// ---------------------------------------------------------------------------
// 第二轮审阅修复回归（§2.50.18-§2.50.20 / §2.50.8）
// ---------------------------------------------------------------------------

use std::sync::atomic::{AtomicUsize, Ordering};

use kiana_ext_api::ui::{
    ExtensionWidgetOptions, FooterFactory, HeaderFactory, OverlayOptionsOrFn, WidgetContent,
    WidgetFactory,
};
use kiana_interactive::components::custom_editor::{CustomEditor, CustomEditorOptions};
use kiana_tui::Terminal;
use kiana_tui::components::editor::EditorTui;
use kiana_tui::tui::OverlayOptions;

/// 同步 setter 必须在同一处理器里对 getter 可见（TS 的 setter 是同步属性写入）。
#[tokio::test(flavor = "current_thread")]
async fn sync_setters_are_visible_to_getters_before_the_ui_loop_processes_them() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        // init 安装 CURRENT_MODE，否则 getter 会走快照回退路径（旧测试的漏洞）。
        fixture.mode.borrow_mut().init().await;
        let ui = context(&fixture.mode);

        ui.set_editor_text("hello");
        assert_eq!(ui.get_editor_text(), "hello");
        assert!(!ui.get_tools_expanded());
        ui.set_tools_expanded(true);
        assert!(ui.get_tools_expanded());
        assert!(ui.get_editor_component().is_none());
        let history = Arc::new(std::sync::Mutex::new(Vec::new()));
        ui.set_editor_component(Some(editor_factory("CUSTOM_EDITOR", history)));
        assert!(ui.get_editor_component().is_some());

        // drain 循环处理后，实时状态与快照都跟上。
        common::wait_until(|| fixture.mode.borrow().test_editor_text() == "hello").await;
        common::wait_until(|| fixture.mode.borrow().test_tool_output_expanded()).await;
        common::wait_until(|| {
            fixture
                .mode
                .borrow()
                .test_editor_slot_text()
                .contains("CUSTOM_EDITOR")
        })
        .await;
        assert_eq!(ui.get_editor_text(), "hello");
        assert!(ui.get_tools_expanded());
        assert!(ui.get_editor_component().is_some());
    })
    .await;
}

/// 构造一个返回真实 `CustomEditor` 的工厂（模拟 TS 里带 `actionHandlers` 的编辑器）。
fn custom_editor_factory(
    configure: impl Fn(&mut CustomEditor) + Send + Sync + 'static,
) -> EditorFactory {
    Arc::new(move |_tui, theme: &EditorTheme, keybindings| {
        let terminal: Rc<RefCell<Box<dyn Terminal>>> =
            Rc::new(RefCell::new(Box::new(common::TestTerminal::new())));
        let mut editor = CustomEditor::new(
            EditorTui::from_terminal(terminal),
            theme.clone(),
            keybindings,
            CustomEditorOptions::default(),
        );
        configure(&mut editor);
        Box::new(editor) as Box<dyn EditorComponent>
    })
}

fn counted(counter: &Arc<AtomicUsize>) -> Box<dyn FnMut()> {
    let counter = Arc::clone(counter);
    Box::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    })
}

/// `setCustomEditorComponent`：CustomEditor 必须继承默认编辑器的 Escape/Ctrl+D/粘贴图片/
/// 扩展快捷键与全部应用动作处理器（否则这些键位在扩展编辑器里静默失效）。
#[tokio::test(flavor = "current_thread")]
async fn set_custom_editor_component_copies_default_editor_handlers() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        fixture.mode.borrow_mut().init().await;

        let escape = Arc::new(AtomicUsize::new(0));
        let ctrl_d = Arc::new(AtomicUsize::new(0));
        let paste_image = Arc::new(AtomicUsize::new(0));
        let shortcut = Arc::new(AtomicUsize::new(0));
        let action = Arc::new(AtomicUsize::new(0));
        {
            let mut mode = fixture.mode.borrow_mut();
            mode.test_set_default_editor_on_escape(counted(&escape));
            mode.test_set_default_editor_on_ctrl_d(counted(&ctrl_d));
            mode.test_set_default_editor_on_paste_image(counted(&paste_image));
            let shortcut_counter = Arc::clone(&shortcut);
            mode.test_set_default_editor_on_extension_shortcut(Box::new(move |data: &str| {
                if data == "!" {
                    shortcut_counter.fetch_add(1, Ordering::SeqCst);
                    true
                } else {
                    false
                }
            }));
            mode.test_set_default_editor_action_handler("app.model.select", counted(&action));
        }

        fixture
            .mode
            .borrow_mut()
            .test_set_custom_editor_component(Some(custom_editor_factory(|_| {})));

        fixture.mode.borrow_mut().test_send_to_active_editor("\x1b");
        assert_eq!(escape.load(Ordering::SeqCst), 1, "escape forwarded");
        fixture.mode.borrow_mut().test_send_to_active_editor("\x04");
        assert_eq!(ctrl_d.load(Ordering::SeqCst), 1, "ctrl+d forwarded");
        fixture.mode.borrow_mut().test_send_to_active_editor("\x16");
        assert_eq!(
            paste_image.load(Ordering::SeqCst),
            1,
            "paste image forwarded"
        );
        fixture.mode.borrow_mut().test_send_to_active_editor("!");
        assert_eq!(
            shortcut.load(Ordering::SeqCst),
            1,
            "extension shortcut forwarded"
        );
        fixture.mode.borrow_mut().test_send_to_active_editor("\x0c");
        assert_eq!(
            action.load(Ordering::SeqCst),
            1,
            "app.model.select forwarded"
        );
    })
    .await;
}

/// 目标编辑器自己已有的回调不会被默认编辑器覆盖（TS 的 `if (!customEditor.onEscape)`）。
#[tokio::test(flavor = "current_thread")]
async fn set_custom_editor_component_keeps_existing_handlers() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        fixture.mode.borrow_mut().init().await;
        let default_escape = Arc::new(AtomicUsize::new(0));
        let own_escape = Arc::new(AtomicUsize::new(0));
        fixture
            .mode
            .borrow_mut()
            .test_set_default_editor_on_escape(counted(&default_escape));

        let own = Arc::clone(&own_escape);
        fixture
            .mode
            .borrow_mut()
            .test_set_custom_editor_component(Some(custom_editor_factory(move |editor| {
                let own = Arc::clone(&own);
                editor.set_on_escape(Some(Box::new(move || {
                    own.fetch_add(1, Ordering::SeqCst);
                })));
            })));

        fixture.mode.borrow_mut().test_send_to_active_editor("\x1b");
        assert_eq!(own_escape.load(Ordering::SeqCst), 1);
        assert_eq!(default_escape.load(Ordering::SeqCst), 0);
    })
    .await;
}

/// `setTheme` 走 InteractiveThemeController：设置 `system` 打开终端明暗跟随，固定主题关闭它，
/// 成功时同步写 settings（TS 的 setThemeName + settingsManager.setTheme）。
#[tokio::test(flavor = "current_thread")]
async fn set_theme_goes_through_the_theme_controller() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        fixture.mode.borrow_mut().init().await;
        let ui = context(&fixture.mode);

        let result = ui.set_theme(kiana_ext_api::ui::ThemeOrName::Name("light".to_string()));
        assert!(result.success, "{result:?}");
        assert_eq!(
            fixture
                .mode
                .borrow()
                .settings_manager()
                .get_theme()
                .as_deref(),
            Some("light"),
            "settings write is synchronous"
        );
        assert!(
            !fixture.mode.borrow().test_scheme_notifications_enabled(),
            "a fixed theme disables terminal appearance auto-sync"
        );

        let result = ui.set_theme(kiana_ext_api::ui::ThemeOrName::Name("system".to_string()));
        assert!(result.success, "{result:?}");
        assert!(
            fixture.mode.borrow().test_scheme_notifications_enabled(),
            "the system theme enables terminal appearance auto-sync"
        );
        assert_eq!(
            fixture
                .mode
                .borrow()
                .settings_manager()
                .get_theme()
                .as_deref(),
            Some("system")
        );

        // 恢复 dark，避免影响同进程其它用例。
        kiana_theme::init_theme(Some("dark"), false);
    })
    .await;
}

/// `resetExtensionUI` 只 dispose 对话框，不结算等待中的 Promise，因此它们保持挂起。
#[tokio::test(flavor = "current_thread")]
async fn reset_extension_ui_leaves_pending_dialogs_unresolved() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        fixture.mode.borrow_mut().init().await;
        let ui = context(&fixture.mode);

        let finished = Arc::new(AtomicUsize::new(0));
        let ui_one = Arc::clone(&ui);
        let finished_one = Arc::clone(&finished);
        let pending = tokio::task::spawn_local(async move {
            let value = ui_one.confirm("Pending?", "still waiting", None).await;
            finished_one.fetch_add(1, Ordering::SeqCst);
            value
        });
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 1).await;

        fixture.mode.borrow_mut().test_reset_extension_ui();
        assert_eq!(fixture.mode.borrow().test_dialog_count(), 0);
        assert_eq!(fixture.mode.borrow().test_editor_slot_child_count(), 1);

        common::yield_tasks(50).await;
        assert_eq!(
            finished.load(Ordering::SeqCst),
            0,
            "the pending dialog must not resolve on reset"
        );
        pending.abort();
        let _ = pending.await;
    })
    .await;
}

// --- overlay / widget 组件的 dispose 与 width 属性 ---

/// 记录输入 / 渲染宽度 / dispose 次数的测试组件（跨工厂共享状态用 Arc）。
struct RecordingComponent {
    label: &'static str,
    inputs: Arc<std::sync::Mutex<Vec<String>>>,
    widths: Arc<std::sync::Mutex<Vec<usize>>>,
    disposed: Arc<AtomicUsize>,
    width: Option<f64>,
}

impl Component for RecordingComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.widths
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(width);
        vec![self.label.to_string()]
    }

    fn handle_input(&mut self, data: &str) {
        self.inputs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(data.to_string());
    }

    fn invalidate(&mut self) {}

    fn dispose(&mut self) {
        self.disposed.fetch_add(1, Ordering::SeqCst);
    }

    fn width(&self) -> Option<f64> {
        self.width
    }
}

type DoneSlot = Arc<std::sync::Mutex<Option<CustomDoneFn>>>;

fn recording_custom_factory(
    label: &'static str,
    inputs: Arc<std::sync::Mutex<Vec<String>>>,
    disposed: Arc<AtomicUsize>,
    done_slot: DoneSlot,
) -> CustomFactory {
    Arc::new(move |_tui, _theme, _keybindings, done| {
        *done_slot.lock().unwrap_or_else(|error| error.into_inner()) = Some(done);
        let inputs = Arc::clone(&inputs);
        let disposed = Arc::clone(&disposed);
        Box::pin(async move {
            Box::new(RecordingComponent {
                label,
                inputs,
                widths: Arc::new(std::sync::Mutex::new(Vec::new())),
                disposed,
                width: None,
            }) as Box<dyn Component>
        })
    })
}

fn overlay_options() -> CustomOptions {
    CustomOptions {
        overlay: Some(true),
        overlay_options: None,
        on_handle: None,
    }
}

/// 内联 custom UI 关闭后，overlay custom UI 重新拿到输入（§2.50.19 焦点恢复）。
#[tokio::test(flavor = "current_thread")]
async fn overlay_custom_ui_reclaims_input_after_inline_custom_ui_closes() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        fixture.mode.borrow_mut().init().await;
        let ui = context(&fixture.mode);

        let overlay_inputs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let overlay_done: DoneSlot = Arc::new(std::sync::Mutex::new(None));
        let overlay_ui = Arc::clone(&ui);
        let overlay_factory = recording_custom_factory(
            "OVERLAY",
            Arc::clone(&overlay_inputs),
            Arc::new(AtomicUsize::new(0)),
            Arc::clone(&overlay_done),
        );
        let overlay_task = tokio::task::spawn_local(async move {
            overlay_ui
                .custom(overlay_factory, Some(overlay_options()))
                .await
        });
        common::wait_until(|| fixture.mode.borrow().test_ui_has_overlay()).await;

        let inline_inputs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let inline_done: DoneSlot = Arc::new(std::sync::Mutex::new(None));
        let inline_ui = Arc::clone(&ui);
        let inline_factory = recording_custom_factory(
            "INLINE",
            Arc::clone(&inline_inputs),
            Arc::new(AtomicUsize::new(0)),
            Arc::clone(&inline_done),
        );
        let inline_task =
            tokio::task::spawn_local(async move { inline_ui.custom(inline_factory, None).await });
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 1).await;

        fixture.mode.borrow_mut().test_ui_handle_input("a");
        assert_eq!(
            inline_inputs
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_slice(),
            ["a"]
        );
        assert!(
            overlay_inputs
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty(),
            "the overlay must not receive input while the inline dialog is focused"
        );

        // 关闭内联对话框。
        let done = inline_done
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .expect("inline done");
        done(Box::new("inline done"));
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 0).await;
        let _ = inline_task.await;

        // 焦点回到 overlay。
        fixture.mode.borrow_mut().test_ui_handle_input("x");
        assert_eq!(
            overlay_inputs
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_slice(),
            ["x"]
        );
        assert_eq!(
            inline_inputs
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_slice(),
            ["a"]
        );

        let done = overlay_done
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .expect("overlay done");
        done(Box::new("overlay done"));
        let _ = overlay_task.await;
        common::wait_until(|| !fixture.mode.borrow().test_ui_has_overlay()).await;
    })
    .await;
}

/// 关闭下层 overlay 只移除它自己的 overlay，不误删栈顶（regression #10648）。
#[tokio::test(flavor = "current_thread")]
async fn closing_a_lower_overlay_keeps_the_topmost_overlay() {
    let _theme = theme();
    common::run_local(async {
        for non_capturing in [false, true] {
            assert_closing_a_lower_overlay(non_capturing).await;
        }
    })
    .await;
}

async fn assert_closing_a_lower_overlay(non_capturing: bool) {
    let fixture = fixture().await;
    fixture.mode.borrow_mut().init().await;
    let ui = context(&fixture.mode);

    let lower_inputs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let lower_done: DoneSlot = Arc::new(std::sync::Mutex::new(None));
    let lower_ui = Arc::clone(&ui);
    let lower_factory = recording_custom_factory(
        "LOWER",
        Arc::clone(&lower_inputs),
        Arc::new(AtomicUsize::new(0)),
        Arc::clone(&lower_done),
    );
    let lower_task = tokio::task::spawn_local(async move {
        lower_ui
            .custom(
                lower_factory,
                Some(CustomOptions {
                    overlay: Some(true),
                    overlay_options: Some(OverlayOptionsOrFn::Static(OverlayOptions {
                        non_capturing: Some(non_capturing),
                        ..OverlayOptions::default()
                    })),
                    on_handle: None,
                }),
            )
            .await
    });
    common::wait_until(|| fixture.mode.borrow().test_ui_has_overlay()).await;

    let upper_inputs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let upper_done: DoneSlot = Arc::new(std::sync::Mutex::new(None));
    let upper_ui = Arc::clone(&ui);
    let upper_factory = recording_custom_factory(
        "UPPER",
        Arc::clone(&upper_inputs),
        Arc::new(AtomicUsize::new(0)),
        Arc::clone(&upper_done),
    );
    let upper_task = tokio::task::spawn_local(async move {
        upper_ui
            .custom(upper_factory, Some(overlay_options()))
            .await
    });
    common::wait_until(|| {
        fixture.mode.borrow_mut().test_ui_has_overlay()
            && upper_done
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_some()
    })
    .await;

    // 关闭下层 overlay。
    let done = lower_done
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
        .expect("lower done");
    done(Box::new("lower closed"));
    let _ = lower_task.await;
    common::yield_tasks(10).await;
    assert!(
        fixture.mode.borrow().test_ui_has_overlay(),
        "the topmost overlay must survive (non_capturing={non_capturing})"
    );

    fixture.mode.borrow_mut().test_ui_handle_input("x");
    assert_eq!(
        upper_inputs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_slice(),
        ["x"],
        "the topmost overlay must keep focus (non_capturing={non_capturing})"
    );
    assert!(
        lower_inputs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
    );

    let done = upper_done
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
        .expect("upper done");
    done(Box::new("upper closed"));
    let _ = upper_task.await;
    fixture.mode.borrow_mut().test_ui_render_now();
    assert!(!fixture.mode.borrow().test_ui_has_overlay());
}

/// 没有 overlayOptions 时 overlay 宽度取自组件的 `width` 属性；
/// 关闭时调用组件的 `dispose()`。
#[tokio::test(flavor = "current_thread")]
async fn custom_overlay_uses_component_width_and_disposes_on_close() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        fixture.mode.borrow_mut().init().await;
        let ui = context(&fixture.mode);

        let widths = Arc::new(std::sync::Mutex::new(Vec::new()));
        let disposed = Arc::new(AtomicUsize::new(0));
        let done_slot: DoneSlot = Arc::new(std::sync::Mutex::new(None));
        let factory = {
            let widths = Arc::clone(&widths);
            let disposed = Arc::clone(&disposed);
            let done_slot = Arc::clone(&done_slot);
            let factory: CustomFactory = Arc::new(move |_tui, _theme, _kb, done| {
                *done_slot.lock().unwrap_or_else(|error| error.into_inner()) = Some(done);
                let widths = Arc::clone(&widths);
                let disposed = Arc::clone(&disposed);
                Box::pin(async move {
                    Box::new(RecordingComponent {
                        label: "WIDE",
                        inputs: Arc::new(std::sync::Mutex::new(Vec::new())),
                        widths,
                        disposed,
                        width: Some(40.0),
                    }) as Box<dyn Component>
                })
            });
            factory
        };
        let task_ui = Arc::clone(&ui);
        let task = tokio::task::spawn_local(async move {
            task_ui.custom(factory, Some(overlay_options())).await
        });
        common::wait_until(|| fixture.mode.borrow().test_ui_has_overlay()).await;
        fixture.mode.borrow_mut().test_ui_render_now();
        assert!(
            widths
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .contains(&40),
            "overlay width fallback must use component.width: {:?}",
            widths.lock().unwrap_or_else(|error| error.into_inner())
        );

        let done = done_slot
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .expect("done");
        done(Box::new("closed"));
        let _ = task.await;
        assert_eq!(disposed.load(Ordering::SeqCst), 1);
    })
    .await;
}

/// widget / footer / header 被替换或清除时调用组件的 `dispose()`。
#[tokio::test(flavor = "current_thread")]
async fn widget_footer_and_header_components_are_disposed_on_replace() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        fixture.mode.borrow_mut().init().await;
        let ui = context(&fixture.mode);

        let widget_disposed = Arc::new(AtomicUsize::new(0));
        let widget_factory = {
            let disposed = Arc::clone(&widget_disposed);
            let factory: WidgetFactory = Arc::new(move |_tui, _theme| {
                Box::new(RecordingComponent {
                    label: "WIDGET",
                    inputs: Arc::new(std::sync::Mutex::new(Vec::new())),
                    widths: Arc::new(std::sync::Mutex::new(Vec::new())),
                    disposed: Arc::clone(&disposed),
                    width: None,
                }) as Box<dyn Component>
            });
            factory
        };
        ui.set_widget(
            "w",
            Some(WidgetContent::Component(Arc::clone(&widget_factory))),
            Some(ExtensionWidgetOptions::default()),
        );
        common::wait_until(|| fixture.mode.borrow().test_widget_text().contains("WIDGET")).await;
        // 同 key 替换 → 旧组件 dispose 一次。
        ui.set_widget(
            "w",
            Some(WidgetContent::Component(Arc::clone(&widget_factory))),
            Some(ExtensionWidgetOptions::default()),
        );
        common::wait_until(|| widget_disposed.load(Ordering::SeqCst) == 1).await;
        // 清除 → 再 dispose 一次。
        ui.set_widget("w", None, None);
        common::wait_until(|| widget_disposed.load(Ordering::SeqCst) == 2).await;

        let footer_disposed = Arc::new(AtomicUsize::new(0));
        let factory: FooterFactory = {
            let disposed = Arc::clone(&footer_disposed);
            Arc::new(move |_tui, _theme, _data| {
                Box::new(RecordingComponent {
                    label: "FOOTER",
                    inputs: Arc::new(std::sync::Mutex::new(Vec::new())),
                    widths: Arc::new(std::sync::Mutex::new(Vec::new())),
                    disposed: Arc::clone(&disposed),
                    width: None,
                }) as Box<dyn Component>
            })
        };
        ui.set_footer(Some(Arc::clone(&factory)));
        common::wait_until(|| footer_disposed.load(Ordering::SeqCst) == 0).await;
        ui.set_footer(Some(factory));
        common::wait_until(|| footer_disposed.load(Ordering::SeqCst) == 1).await;
        ui.set_footer(None);
        common::wait_until(|| footer_disposed.load(Ordering::SeqCst) == 2).await;

        let header_disposed = Arc::new(AtomicUsize::new(0));
        let factory: HeaderFactory = {
            let disposed = Arc::clone(&header_disposed);
            Arc::new(move |_tui, _theme| {
                Box::new(RecordingComponent {
                    label: "HEADER",
                    inputs: Arc::new(std::sync::Mutex::new(Vec::new())),
                    widths: Arc::new(std::sync::Mutex::new(Vec::new())),
                    disposed: Arc::clone(&disposed),
                    width: None,
                }) as Box<dyn Component>
            })
        };
        ui.set_header(Some(Arc::clone(&factory)));
        common::yield_tasks(20).await;
        ui.set_header(Some(factory));
        common::wait_until(|| header_disposed.load(Ordering::SeqCst) == 1).await;
        ui.set_header(None);
        common::wait_until(|| header_disposed.load(Ordering::SeqCst) == 2).await;
    })
    .await;
}

// ---------------------------------------------------------------------------
// clear-on-shrink 状态间距（interactive-tui.test.ts）
// ---------------------------------------------------------------------------

/// 所有状态指示器都走编辑器内嵌 opt-in（默认编辑器内嵌；扩展编辑器走独立状态行）。
#[tokio::test(flavor = "current_thread")]
async fn every_status_indicator_respects_the_editor_embed_opt_in() {
    use kiana_interactive::components::status_indicator::StatusIndicatorKind;

    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        fixture.mode.borrow_mut().init().await;

        for kind in [
            StatusIndicatorKind::Working,
            StatusIndicatorKind::Compaction,
            StatusIndicatorKind::BranchSummary,
            StatusIndicatorKind::Retry,
        ] {
            fixture.mode.borrow_mut().test_show_status_kind(kind);
            assert_eq!(fixture.mode.borrow().test_active_status_kind(), Some(kind));
            assert!(
                fixture
                    .mode
                    .borrow()
                    .test_active_working_indicator_embedded(),
                "{kind:?} must be embedded by the default editor"
            );
            assert_eq!(fixture.mode.borrow().test_status_container_child_count(), 0);
        }

        // 扩展编辑器不支持内嵌 → 所有 kind 都落在独立状态行。
        let history = Arc::new(std::sync::Mutex::new(Vec::new()));
        fixture
            .mode
            .borrow_mut()
            .test_set_custom_editor_component(Some(editor_factory("CUSTOM_EDITOR", history)));
        for kind in [
            StatusIndicatorKind::Working,
            StatusIndicatorKind::Compaction,
            StatusIndicatorKind::BranchSummary,
            StatusIndicatorKind::Retry,
        ] {
            fixture.mode.borrow_mut().test_show_status_kind(kind);
            assert!(
                !fixture
                    .mode
                    .borrow()
                    .test_active_working_indicator_embedded()
            );
            assert_eq!(fixture.mode.borrow().test_status_container_child_count(), 1);
        }
        fixture
            .mode
            .borrow_mut()
            .test_set_custom_editor_component(None);
    })
    .await;
}

/// 清掉内嵌指示器不预留独立状态行；非内嵌指示器在 regular + clearOnShrink 下补一行空状态。
#[tokio::test(flavor = "current_thread")]
async fn clear_status_indicator_reserves_height_only_for_standalone_rows() {
    use kiana_interactive::components::status_indicator::StatusIndicatorKind;

    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        fixture.mode.borrow_mut().init().await;
        fixture.mode.borrow_mut().test_set_clear_on_shrink(true);

        // 内嵌：清掉后不补空行（所有 kind）。
        for kind in [
            StatusIndicatorKind::Working,
            StatusIndicatorKind::Compaction,
            StatusIndicatorKind::BranchSummary,
            StatusIndicatorKind::Retry,
        ] {
            fixture.mode.borrow_mut().test_show_status_kind(kind);
            assert!(
                fixture
                    .mode
                    .borrow()
                    .test_active_working_indicator_embedded()
            );
            fixture.mode.borrow_mut().test_clear_status_indicator(None);
            assert_eq!(fixture.mode.borrow().test_status_container_child_count(), 0);
        }

        // 非内嵌 + regular + clearOnShrink：补一行 IdleStatus。
        let history = Arc::new(std::sync::Mutex::new(Vec::new()));
        fixture
            .mode
            .borrow_mut()
            .test_set_custom_editor_component(Some(editor_factory("CUSTOM_EDITOR", history)));
        fixture
            .mode
            .borrow_mut()
            .test_show_status_kind(StatusIndicatorKind::Working);
        assert_eq!(fixture.mode.borrow().test_status_container_child_count(), 1);
        fixture.mode.borrow_mut().test_clear_status_indicator(None);
        assert_eq!(
            fixture.mode.borrow().test_status_container_child_count(),
            1,
            "regular + clearOnShrink keeps one idle status row"
        );
        fixture
            .mode
            .borrow_mut()
            .test_set_custom_editor_component(None);
    })
    .await;
}

// ---------------------------------------------------------------------------
// 右键粘贴：括号粘贴序列只投递给当前聚焦的槽位组件（editor / 扩展对话框）
// ---------------------------------------------------------------------------

struct StaticClipboard {
    text: Option<String>,
}

#[async_trait::async_trait(?Send)]
impl kiana_interactive::interactive_mode::input::ClipboardSource for StaticClipboard {
    async fn read_file_paths(&self) -> Result<Option<Vec<String>>, String> {
        Ok(None)
    }
    async fn read_image(
        &self,
    ) -> Result<Option<kiana_core::image_process::ClipboardImage>, String> {
        Ok(None)
    }
    async fn read_text(&self) -> Result<Option<String>, String> {
        Ok(self.text.clone())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn right_click_paste_feeds_the_focused_editor_a_bracketed_paste() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        fixture.mode.borrow_mut().init().await;
        fixture.mode.borrow_mut().test_set_editor_text("abc");

        let clipboard = StaticClipboard {
            text: Some("clipboard text".to_string()),
        };
        fixture
            .mode
            .borrow_mut()
            .test_handle_right_click_paste_with(&clipboard)
            .await;
        let text = fixture.mode.borrow().test_editor_text();
        assert!(text.contains("clipboard text"), "{text:?}");
        assert!(!text.contains("[200~"), "{text:?}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn right_click_paste_targets_the_focused_extension_dialog() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        fixture.mode.borrow_mut().init().await;
        fixture.mode.borrow_mut().test_set_editor_text("draft");

        let inputs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let done_slot: DoneSlot = Arc::new(std::sync::Mutex::new(None));
        let ui = context(&fixture.mode);
        let factory = recording_custom_factory(
            "INLINE",
            Arc::clone(&inputs),
            Arc::new(AtomicUsize::new(0)),
            Arc::clone(&done_slot),
        );
        let task = tokio::task::spawn_local(async move { ui.custom(factory, None).await });
        common::wait_until(|| fixture.mode.borrow().test_dialog_count() == 1).await;

        let clipboard = StaticClipboard {
            text: Some("dialog text".to_string()),
        };
        fixture
            .mode
            .borrow_mut()
            .test_handle_right_click_paste_with(&clipboard)
            .await;
        let inputs = inputs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        assert_eq!(inputs, ["\x1b[200~dialog text\x1b[201~"]);
        assert_eq!(
            fixture.mode.borrow().test_editor_text(),
            "draft",
            "the hidden editor must not receive the paste"
        );

        let done = done_slot
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .expect("done");
        done(Box::new("done"));
        let _ = task.await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn right_click_paste_targets_the_focused_overlay_component() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        fixture.mode.borrow_mut().init().await;
        fixture.mode.borrow_mut().test_set_editor_text("draft");

        let inputs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let done_slot: DoneSlot = Arc::new(std::sync::Mutex::new(None));
        let ui = context(&fixture.mode);
        let factory = recording_custom_factory(
            "OVERLAY",
            Arc::clone(&inputs),
            Arc::new(AtomicUsize::new(0)),
            Arc::clone(&done_slot),
        );
        let task =
            tokio::task::spawn_local(
                async move { ui.custom(factory, Some(overlay_options())).await },
            );
        common::wait_until(|| fixture.mode.borrow().test_ui_has_overlay()).await;
        assert!(
            matches!(
                fixture.mode.borrow().test_ui_focus(),
                Some(kiana_tui::tui::FocusTarget::Overlay { .. })
            ),
            "the overlay captures focus when it opens"
        );

        let clipboard = StaticClipboard {
            text: Some("overlay text".to_string()),
        };
        fixture
            .mode
            .borrow_mut()
            .test_handle_right_click_paste_with(&clipboard)
            .await;
        let inputs = inputs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        assert_eq!(
            inputs,
            ["\x1b[200~overlay text\x1b[201~"],
            "paste must go to the focused overlay, not the editor slot"
        );
        assert_eq!(
            fixture.mode.borrow().test_editor_text(),
            "draft",
            "the hidden editor must not receive the paste"
        );

        let done = done_slot
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .expect("done");
        done(Box::new("done"));
        let _ = task.await;
    })
    .await;
}

/// `setToolsExpanded` 同步刷新内置 header 与 loaded-resources 容器里的可展开组件。
#[tokio::test(flavor = "current_thread")]
async fn set_tools_expanded_applies_to_the_active_header_and_loaded_resources() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        fixture.mode.borrow_mut().init().await;
        let probe = fixture.mode.borrow_mut().test_push_expandable_probe();
        let initial = fixture
            .mode
            .borrow()
            .test_built_in_header_expanded()
            .expect("built-in header");
        assert!(!probe(), "probe starts collapsed");

        fixture.mode.borrow_mut().test_set_tools_expanded(!initial);
        assert_eq!(
            fixture.mode.borrow().test_built_in_header_expanded(),
            Some(!initial)
        );
        assert!(probe(), "loaded-resources children must follow");
        let rendered = fixture.mode.borrow().chat_render_text();
        assert!(
            rendered.contains(if !initial {
                "Tool output: expanded"
            } else {
                "Tool output: collapsed"
            }),
            "{rendered}"
        );

        fixture.mode.borrow_mut().test_set_tools_expanded(initial);
        assert_eq!(
            fixture.mode.borrow().test_built_in_header_expanded(),
            Some(initial)
        );
        assert!(!probe());
    })
    .await;
}

/// 全屏 + 未内嵌的指示器清除后不预留独立状态行（TS: expectedChildren 0）。
#[tokio::test(flavor = "current_thread")]
async fn clear_status_indicator_reserves_no_row_in_fullscreen_for_a_custom_editor() {
    use kiana_interactive::components::status_indicator::StatusIndicatorKind;

    let _theme = theme();
    common::run_local(async {
        let fixture = fullscreen_fixture().await;
        fixture.mode.borrow_mut().init().await;
        fixture.mode.borrow_mut().test_set_clear_on_shrink(true);
        let history = Arc::new(std::sync::Mutex::new(Vec::new()));
        fixture
            .mode
            .borrow_mut()
            .test_set_custom_editor_component(Some(editor_factory("CUSTOM_EDITOR", history)));

        fixture
            .mode
            .borrow_mut()
            .test_show_status_kind(StatusIndicatorKind::Working);
        assert!(
            !fixture
                .mode
                .borrow()
                .test_active_working_indicator_embedded()
        );
        assert_eq!(fixture.mode.borrow().test_status_container_child_count(), 1);
        fixture.mode.borrow_mut().test_clear_status_indicator(None);
        assert_eq!(
            fixture.mode.borrow().test_status_container_child_count(),
            0,
            "fullscreen must not reserve a standalone status row"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// `interactive-mode-status.test.ts`：autocomplete wrapper factories
// ---------------------------------------------------------------------------

/// 最小 provider：只用来携带 triggerCharacters（其余方法不参与断言）。
struct TriggerProvider {
    triggers: Vec<char>,
}

impl kiana_tui::autocomplete::AutocompleteProvider for TriggerProvider {
    fn trigger_characters(&self) -> Option<Vec<char>> {
        Some(self.triggers.clone())
    }

    fn get_suggestions<'a>(
        &'a mut self,
        _lines: &'a [String],
        _cursor_line: usize,
        _cursor_col: usize,
        _options: &'a kiana_tui::autocomplete::GetSuggestionsOptions,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Option<kiana_tui::autocomplete::AutocompleteSuggestions>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async { None })
    }

    fn apply_completion(
        &self,
        _lines: &[String],
        _cursor_line: usize,
        _cursor_col: usize,
        _item: &kiana_tui::autocomplete::AutocompleteItem,
        _prefix: &str,
    ) -> kiana_tui::autocomplete::ApplyCompletionResult {
        kiana_tui::autocomplete::ApplyCompletionResult {
            lines: Vec::new(),
            cursor_line: 0,
            cursor_col: 0,
        }
    }
}

/// `stacks wrapper factories over a fresh base provider` +
/// `merges triggerCharacters from wrapper factories`。
#[tokio::test(flavor = "current_thread")]
async fn stacks_wrapper_factories_and_merges_their_trigger_characters() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture().await;
        let order: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

        let first: kiana_ext_api::ui::AutocompleteProviderFactory = {
            let order = Arc::clone(&order);
            Arc::new(
                move |_inner: Box<dyn kiana_tui::autocomplete::AutocompleteProvider>| {
                    order.lock().unwrap().push(1);
                    Box::new(TriggerProvider {
                        triggers: vec!['@', '#'],
                    }) as Box<dyn kiana_tui::autocomplete::AutocompleteProvider>
                },
            )
        };
        let second: kiana_ext_api::ui::AutocompleteProviderFactory = {
            let order = Arc::clone(&order);
            Arc::new(
                move |_inner: Box<dyn kiana_tui::autocomplete::AutocompleteProvider>| {
                    order.lock().unwrap().push(2);
                    Box::new(TriggerProvider {
                        triggers: vec!['@', '!'],
                    }) as Box<dyn kiana_tui::autocomplete::AutocompleteProvider>
                },
            )
        };

        let triggers = fixture
            .mode
            .borrow_mut()
            .test_autocomplete_trigger_characters(vec![first, second]);
        // `[...new Set(...)]`：保持首次出现顺序。
        assert_eq!(triggers, Some(vec!['@', '#', '!']));
        assert_eq!(&*order.lock().unwrap(), &[1, 2]);
    })
    .await;
}
