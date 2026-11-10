//! `noOpUIContext` 的默认值（`runner.ts:331-362`，`ca-extensions.md` §2.3.6）逐成员断言。
//!
//! 这个上下文是进程级单例（`ExtensionRunner::has_ui` 用 `Arc::ptr_eq` 与它比较），因此测试
//! 同时钉住“同一个 `Arc`”与每个成员的逐字默认值。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use kiana_ext_api::ui::{
    AutocompleteProviderFactory, CustomFactory, CustomOptions, EditorFactory,
    ExtensionUIDialogOptions, ExtensionWidgetOptions, FooterFactory, HeaderFactory, NotifyKind,
    TerminalInputHandler, ThemeOrName, ThemeResult, WidgetContent, WorkingIndicatorOptions,
};
use kiana_extensions::runner::noop_ui::{UI_NOT_AVAILABLE, is_no_op_ui_context, no_op_ui_context};
use kiana_extensions::runner::ui_prompt::{UiPromptContext, UiPromptHost};
use kiana_tui::tui::Component;

/// `kiana_theme` 的全局主题是进程级的；改动全局状态的测试与读取 `theme` 的测试串行化。
static THEME_LOCK: Mutex<()> = Mutex::new(());

fn custom_factory() -> CustomFactory {
    Arc::new(|_tui, _theme, _keybindings, _done| {
        // no-op 上下文不得调用工厂。
        Box::pin(std::future::pending())
    })
}

fn editor_factory() -> EditorFactory {
    Arc::new(
        |_tui, _theme, _keybindings| -> Box<dyn kiana_tui::editor_component::EditorComponent> {
            unreachable!("no-op context must not invoke the editor factory")
        },
    )
}

fn footer_factory() -> FooterFactory {
    Arc::new(|_tui, _theme, _footer| -> Box<dyn Component> {
        unreachable!("no-op context must not invoke the footer factory")
    })
}

fn header_factory() -> HeaderFactory {
    Arc::new(|_tui, _theme| -> Box<dyn Component> {
        unreachable!("no-op context must not invoke the header factory")
    })
}

fn autocomplete_factory() -> AutocompleteProviderFactory {
    Arc::new(|provider| provider)
}

#[test]
fn is_a_process_global_singleton() {
    let first = no_op_ui_context();
    let second = no_op_ui_context();
    assert!(Arc::ptr_eq(&first, &second));
    assert!(is_no_op_ui_context(&first));
    assert!(is_no_op_ui_context(&second));
}

#[test]
fn is_distinguishable_from_a_wrapped_or_real_ui_context() {
    struct NoEvents;
    impl UiPromptHost for NoEvents {
        fn emit_ui_prompt(&self, _event: kiana_ext_api::events::ExtensionEvent) {}
    }

    let wrapped = UiPromptContext::wrap(no_op_ui_context(), Arc::new(NoEvents));
    assert!(!is_no_op_ui_context(&wrapped));
}

/// 逐成员核对默认值：`undefined`/`false`/`""`/`[]`/空操作/`UI not available`。
#[tokio::test(flavor = "current_thread")]
async fn matches_the_ts_noop_defaults_member_by_member() {
    let ui = no_op_ui_context();

    // 阻塞式对话框。
    assert_eq!(
        ui.select("Pick", &["a".to_owned(), "b".to_owned()], None)
            .await,
        None
    );
    assert_eq!(
        ui.select(
            "Pick",
            &[],
            Some(ExtensionUIDialogOptions {
                signal: None,
                timeout: Some(1_000.0),
            }),
        )
        .await,
        None
    );
    assert!(!ui.confirm("Title", "Message", None).await);
    assert_eq!(ui.input("Title", Some("placeholder"), None).await, None);
    assert_eq!(ui.editor("Title", Some("prefill")).await, None);
    let custom = ui
        .custom(
            custom_factory(),
            Some(CustomOptions {
                overlay: None,
                overlay_options: None,
                on_handle: None,
            }),
        )
        .await;
    assert!(
        custom.downcast::<()>().is_ok(),
        "custom() must resolve to `()`"
    );

    // 通知与终端输入。
    ui.notify("hello", None);
    ui.notify("hello", Some(NotifyKind::Warning));
    ui.notify("hello", Some(NotifyKind::Error));
    let called = Arc::new(AtomicUsize::new(0));
    let called_in_handler = Arc::clone(&called);
    let handler: TerminalInputHandler = Arc::new(move |_data| {
        called_in_handler.fetch_add(1, Ordering::SeqCst);
        None
    });
    let unsubscribe = ui.on_terminal_input(handler);
    unsubscribe();
    assert_eq!(called.load(Ordering::SeqCst), 0);

    // 状态/消息/指示器：全部空操作。
    ui.set_status("key", Some("text"));
    ui.set_status("key", None);
    ui.set_working_message(Some("working"));
    ui.set_working_message(None);
    ui.set_working_visible(true);
    ui.set_working_indicator(Some(WorkingIndicatorOptions {
        frames: Some(Vec::new()),
        interval_ms: None,
    }));
    ui.set_working_indicator(None);
    ui.set_hidden_thinking_label(Some("thinking"));
    ui.set_hidden_thinking_label(None);
    ui.set_widget(
        "key",
        Some(WidgetContent::Lines(vec!["line".to_owned()])),
        Some(ExtensionWidgetOptions::default()),
    );
    ui.set_widget("key", None, None);
    ui.set_footer(None);
    ui.set_footer(Some(footer_factory()));
    ui.set_header(None);
    ui.set_header(Some(header_factory()));
    ui.set_title("Title");

    // 编辑器。
    ui.paste_to_editor("text");
    ui.set_editor_text("text");
    assert_eq!(ui.get_editor_text(), "");
    ui.add_autocomplete_provider(autocomplete_factory());
    ui.set_editor_component(None);
    ui.set_editor_component(Some(editor_factory()));
    assert!(ui.get_editor_component().is_none());

    // 主题。
    assert!(ui.get_all_themes().is_empty());
    assert!(ui.get_theme("dark").is_none());
    let result = ui.set_theme(ThemeOrName::Name("dark".to_owned()));
    assert_eq!(
        result,
        ThemeResult {
            success: false,
            error: Some(UI_NOT_AVAILABLE.to_owned()),
        }
    );
    assert_eq!(result.error.as_deref(), Some("UI not available"));
    // 主题：`theme` 读当前全局主题。TS 的 `get theme() { return theme }` 返回 theme.ts 的
    // 稳定 Proxy；Rust 的 `ExtensionUiContext::theme` 是冻结的 `&Theme`（无法表达“未初始化
    // 即抛错”，见报告 deviations），这里只固定“读到当前主题 + 同一主题引用稳定”。
    let _guard = THEME_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    kiana_theme::theme::init_theme(Some("dark"), false);
    let theme = ui.theme();
    assert_eq!(theme.name(), Some("dark"));
    assert!(std::ptr::eq(theme, ui.theme()));
    let current = kiana_theme::theme::current_theme().expect("theme initialized");
    assert!(std::ptr::eq(theme, current.as_ref()));

    // 工具展开状态。
    assert!(!ui.get_tools_expanded());
    ui.set_tools_expanded(true);
    assert!(!ui.get_tools_expanded());
}

/// TS: `get theme() { return theme }` —— 每次读取当前全局主题，而不是首次访问的快照。
#[test]
fn theme_reads_the_current_global_theme_on_each_access() {
    let _guard = THEME_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let ui = no_op_ui_context();
    kiana_theme::theme::init_theme(Some("dark"), false);
    assert_eq!(ui.theme().name(), Some("dark"));
    kiana_theme::theme::init_theme(Some("light"), false);
    assert_eq!(ui.theme().name(), Some("light"));
    kiana_theme::theme::init_theme(Some("dark"), false);
    assert_eq!(ui.theme().name(), Some("dark"));
}
