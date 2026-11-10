//! `index.rs` 再导出面的编译/可用性测试。

mod common;

use kiana_tui::Component;
use kiana_ui_kit::{
    AuthUrlComponent, BorderedLoader, BorderedLoaderOptions, CountdownTimer, DynamicBorder, Keep,
    KeyTextFormatOptions, RenderDiffOptions, ThemedText, VisualLinePreview,
    VisualLinePreviewOptions, VisualTruncateResult, format_key_text, key_display_text, key_hint,
    key_text, kiana_logo_lines, kiana_wordmark, raw_key_hint, render_diff, supports_kiana_logo,
    truncate_to_visual_lines,
};

#[test]
fn barrel_reexports_every_public_item() {
    common::setup();

    let mut border = DynamicBorder::default();
    assert_eq!(border.render(8).len(), 1);

    let diff = render_diff("+1 x", Some(&RenderDiffOptions::default()));
    assert!(diff.contains("x"));

    assert_eq!(
        format_key_text("ctrl+x", KeyTextFormatOptions::default()),
        "ctrl+x"
    );
    assert_eq!(key_text("tui.select.cancel"), "escape/ctrl+c");
    assert_eq!(key_display_text("tui.select.cancel"), "Escape/Ctrl+C");
    assert!(key_hint("app.message.copy", "to copy").contains("ctrl+x"));
    assert!(raw_key_hint("ctrl+x", "to copy").contains("ctrl+x"));

    let result: VisualTruncateResult = truncate_to_visual_lines("a\nb", 1, 10, 0, Keep::End);
    assert_eq!(result.skipped_count, 1);

    let mut preview = VisualLinePreview::new(VisualLinePreviewOptions {
        text: "a\nb".to_string(),
        max_visual_lines: 1,
        keep: Keep::Start,
        format_hint: Box::new(|hidden| format!("... {hidden} more")),
    });
    assert_eq!(preview.render(20).len(), 2);

    let logo: [String; 2] = kiana_logo_lines();
    assert_eq!(logo.len(), 2);
    assert_eq!(kiana_tui::visible_width(&kiana_wordmark()), 5);
    let _ = supports_kiana_logo();

    let mut themed = ThemedText::new(Box::new(|| "hello".to_string()));
    assert!(!themed.render(10).is_empty());

    let auth = AuthUrlComponent::new(None, "https://example.invalid");
    assert_eq!(auth.url, "https://example.invalid");

    let mut countdown = CountdownTimer::new(1000.0, None, Box::new(|_| {}), Box::new(|| {}));
    assert_eq!(countdown.remaining_seconds(), 1.0);
    countdown.dispose();

    let mut loader = BorderedLoader::new(
        None,
        common::current_theme(),
        "Loading...",
        Some(BorderedLoaderOptions { cancellable: false }),
    );
    assert!(!loader.render(20).is_empty());

    // 模块路径仍然可用（与 crate 根再导出并存）。
    let _: fn(&str, Option<&kiana_ui_kit::diff::RenderDiffOptions>) -> String =
        kiana_ui_kit::diff::render_diff;
    let _ = kiana_ui_kit::dynamic_border::DynamicBorder::default();
    let _ = kiana_ui_kit::keybinding_hints::key_text("tui.select.cancel");
    let _ = kiana_ui_kit::visual_truncate::Keep::Start;
    let _ = kiana_ui_kit::kiana_logo::kiana_logo_lines();
    let _ = kiana_ui_kit::themed_text::ThemedText::new(Box::new(String::new));
    let _ = kiana_ui_kit::auth_url::AuthUrlComponent::new(None, "u");
    let _ = kiana_ui_kit::countdown_timer::CountdownTimer::new(
        1.0,
        None,
        Box::new(|_| {}),
        Box::new(|| {}),
    );
    let _ = kiana_ui_kit::bordered_loader::BorderedLoaderOptions::default();
}
