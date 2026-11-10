//! `ThemedText` 的测试：直接移植 TS `themed-text.test.ts`。

mod common;

use std::cell::Cell;
use std::rc::Rc;

use kiana_theme::ThemeColor;
use kiana_tui::Component;
use kiana_ui_kit::ThemedText;

#[test]
fn builds_lazily_and_rebuilds_with_the_current_theme_after_invalidation() {
    let _guard = common::theme_lock();
    common::setup();
    kiana_theme::init_theme(Some("dark"), false);

    let builds = Rc::new(Cell::new(0usize));
    let counter = Rc::clone(&builds);
    let mut text = ThemedText::new(Box::new(move || {
        counter.set(counter.get() + 1);
        common::fg(ThemeColor::Accent, "hello")
    }));

    // 首次 render 才调用 build。
    assert_eq!(builds.get(), 0);
    let dark: String = text.render(20).join("");

    // 主题切换但未 invalidate：仍返回旧字符串（同一缓存）。
    kiana_theme::init_theme(Some("light"), false);
    assert_eq!(text.render(20).join(""), dark);

    // invalidate 之后用新主题重新构建。
    text.invalidate();
    let light = text.render(20).join("");
    assert!(light.contains(&common::fg_ansi(ThemeColor::Accent)));
    assert_eq!(builds.get(), 2);

    kiana_theme::init_theme(Some("dark"), false);
}

#[test]
fn defaults_to_padding_one_and_renders_within_width() {
    let _guard = common::theme_lock();
    common::setup();
    kiana_theme::init_theme(Some("dark"), false);

    let mut text = ThemedText::new(Box::new(|| common::fg(ThemeColor::Muted, "hello")));
    let lines = text.render(20);
    assert_eq!(
        lines.len(),
        3,
        "paddingY = 1 gives a blank line above and below"
    );
    assert_eq!(kiana_tui::visible_width(&lines[0]), 20);
    assert_eq!(kiana_tui::visible_width(&lines[1]), 20);
    assert_eq!(kiana_tui::visible_width(&lines[2]), 20);
    assert!(kiana_tui::strip_terminal_sequences(&lines[1]).contains(" hello "));

    assert!(!text.handles_input());

    let mut with_padding =
        ThemedText::with_padding(Box::new(|| common::fg(ThemeColor::Muted, "hello")), 0, 0);
    let lines = with_padding.render(20);
    assert_eq!(lines.len(), 1);
    assert_eq!(
        kiana_tui::strip_terminal_sequences(&lines[0]).trim_end(),
        "hello"
    );

    for width in common::WIDTHS {
        let mut component =
            ThemedText::new(Box::new(|| common::fg(ThemeColor::Muted, "hello world")));
        common::assert_width_invariant(&mut component, &[width]);
    }
}

#[test]
fn build_runs_once_per_invalidate_even_across_widths() {
    let _guard = common::theme_lock();
    common::setup();
    kiana_theme::init_theme(Some("dark"), false);

    let builds = Rc::new(Cell::new(0usize));
    let counter = Rc::clone(&builds);
    let mut text = ThemedText::with_padding(
        Box::new(move || {
            counter.set(counter.get() + 1);
            common::fg(ThemeColor::Text, "hello world")
        }),
        0,
        0,
    );

    let _ = text.render(20);
    let _ = text.render(80);
    assert_eq!(builds.get(), 1, "stale flag only rebuilds once");
    text.invalidate();
    let _ = text.render(40);
    assert_eq!(builds.get(), 2);
}
