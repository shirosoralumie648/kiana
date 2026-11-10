//! print/json 的 UI 上下文：与 `runner::noop_ui` 的单一实现逐成员一致
//! （设计文档 §4.5、`ca-extensions.md` §2.3.6）。
//!
//! 关键不变量：`ui::print_json::print_json_ui_context()` 返回的 `Arc` 与
//! `runner::noop_ui::no_op_ui_context()` **指针相等**（`ExtensionRunner::has_ui` 的
//! `Arc::ptr_eq` 判定依赖它）。

use kiana_ext_api::ui::{
    ExtensionUiContext, NotifyKind, ThemeOrName, WidgetContent, WorkingIndicatorOptions,
};
use kiana_extensions::runner::noop_ui::{UI_NOT_AVAILABLE, is_no_op_ui_context, no_op_ui_context};
use kiana_extensions::ui::print_json::{
    UI_NOT_AVAILABLE as PRINT_UI_NOT_AVAILABLE, print_json_ui_context,
};

#[test]
fn print_json_context_is_the_noop_singleton() {
    let print = print_json_ui_context();
    let noop = no_op_ui_context();
    assert!(std::sync::Arc::ptr_eq(&print, &noop));
    assert!(is_no_op_ui_context(&print));
    assert_eq!(PRINT_UI_NOT_AVAILABLE, UI_NOT_AVAILABLE);
    assert_eq!(PRINT_UI_NOT_AVAILABLE, "UI not available");
}

#[tokio::test(flavor = "current_thread")]
async fn print_json_defaults_match_the_ts_noop_context() {
    let ui: std::sync::Arc<dyn ExtensionUiContext> = print_json_ui_context();

    assert_eq!(ui.select("t", &["a".to_owned()], None).await, None);
    assert!(!ui.confirm("t", "m", None).await);
    assert_eq!(ui.input("t", Some("p"), None).await, None);
    assert_eq!(ui.editor("t", Some("prefill")).await, None);
    assert_eq!(ui.get_editor_text(), "");
    assert!(ui.get_editor_component().is_none());
    assert!(ui.get_all_themes().is_empty());
    assert!(ui.get_theme("dark").is_none());
    assert!(!ui.get_tools_expanded());

    let result = ui.set_theme(ThemeOrName::Name("light".to_owned()));
    assert!(!result.success);
    assert_eq!(result.error.as_deref(), Some(UI_NOT_AVAILABLE));

    // 其余成员是空操作（不 panic、无副作用）。
    ui.notify("msg", Some(NotifyKind::Error));
    ui.set_status("k", Some("v"));
    ui.set_status("k", None);
    ui.set_working_message(Some("working"));
    ui.set_working_visible(true);
    ui.set_working_indicator(Some(WorkingIndicatorOptions {
        frames: Some(vec!["a".to_owned()]),
        interval_ms: Some(80.0),
    }));
    ui.set_hidden_thinking_label(Some("hidden"));
    ui.set_widget(
        "w",
        Some(WidgetContent::Lines(vec!["line".to_owned()])),
        None,
    );
    ui.set_footer(None);
    ui.set_header(None);
    ui.set_title("title");
    ui.paste_to_editor("pasted");
    ui.set_editor_text("text");
    ui.set_tools_expanded(true);
    assert!(!ui.get_tools_expanded());

    let unsubscribe = ui.on_terminal_input(std::sync::Arc::new(|_data: &str| None));
    unsubscribe();

    // `custom` 的 TS 语义是 `undefined as never`：Rust 擦除成 `()`。
    let factory: kiana_ext_api::ui::CustomFactory =
        std::sync::Arc::new(|_tui, _theme, _keybindings, _done| {
            unreachable!("the no-op context must not invoke custom factories")
        });
    let value = ui.custom(factory, None).await;
    assert!(value.is::<()>());
}
