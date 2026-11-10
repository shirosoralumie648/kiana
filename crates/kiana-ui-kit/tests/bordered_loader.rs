//! `BorderedLoader` 的组件级测试。

mod common;

use std::cell::Cell;
use std::rc::Rc;

use kiana_theme::ThemeColor;
use kiana_tui::Component;
use kiana_ui_kit::{BorderedLoader, BorderedLoaderOptions};

#[test]
fn cancellable_layout_and_hint() {
    let _guard = common::theme_lock();
    common::setup();
    let (request_render, _renders) = common::request_render();
    let mut loader = BorderedLoader::new(
        Some(request_render),
        common::current_theme(),
        "Working...",
        None,
    );

    assert!(loader.is_cancellable(), "cancellable defaults to true");
    assert_eq!(
        loader.container_children().map(|children| children.len()),
        Some(6),
        "border, loader, spacer, hint, spacer, border"
    );

    let lines = loader.render(80);
    assert_eq!(
        lines.len(),
        7,
        "1 border + 2 loader + 2 spacers + hint + border"
    );
    let plain = common::render_plain(&mut loader, 80);
    assert!(plain.contains("Working..."), "plain: {plain:?}");
    assert!(plain.contains("escape/ctrl+c cancel"), "plain: {plain:?}");

    let signal = loader.signal();
    assert!(!signal.is_cancelled());
}

#[test]
fn cancellable_forwards_input_and_cancels_signal() {
    let _guard = common::theme_lock();
    common::setup();
    let mut loader = BorderedLoader::new(None, common::current_theme(), "Login", None);
    let signal = loader.signal();
    let aborted = Rc::new(Cell::new(false));
    let sink = Rc::clone(&aborted);
    loader.set_on_abort(Some(Box::new(move || sink.set(true))));

    loader.handle_input("x");
    assert!(!signal.is_cancelled());
    assert!(!aborted.get());

    loader.handle_input("\x1b");
    assert!(signal.is_cancelled());
    assert!(aborted.get());
}

#[test]
fn non_cancellable_ignores_input_and_has_no_hint() {
    let _guard = common::theme_lock();
    common::setup();
    let mut loader = BorderedLoader::new(
        None,
        common::current_theme(),
        "Working...",
        Some(BorderedLoaderOptions { cancellable: false }),
    );

    assert!(!loader.is_cancellable());
    assert_eq!(
        loader.container_children().map(|children| children.len()),
        Some(4),
        "border, loader, spacer, border"
    );
    let plain = common::render_plain(&mut loader, 80);
    assert!(!plain.contains("cancel"), "plain: {plain:?}");

    let signal = loader.signal();
    loader.handle_input("\x1b");
    loader.handle_input("x");
    assert!(
        !signal.is_cancelled(),
        "plain loader has a never-aborted signal"
    );
}

#[test]
fn dispose_is_idempotent_and_stops_the_timer() {
    let _guard = common::theme_lock();
    common::setup();
    for cancellable in [true, false] {
        let mut loader = BorderedLoader::new(
            None,
            common::current_theme(),
            "Working...",
            Some(BorderedLoaderOptions { cancellable }),
        );
        let (timer, handle) = common::fake_timer();
        loader.set_timer(Box::new(timer));
        assert!(handle.is_pending(), "loader animation scheduled");

        loader.dispose();
        loader.dispose();
        assert!(!handle.is_pending());
        assert!(handle.cancel_count() >= 1);
        // dispose 之后仍可安全渲染。
        let _ = loader.render(20);
    }
}

#[test]
fn timer_ticks_request_render_through_the_proxy() {
    let _guard = common::theme_lock();
    common::setup();
    let (request_render, renders) = common::request_render();
    let mut loader = BorderedLoader::new(Some(request_render), common::current_theme(), "M", None);
    let (timer, handle) = common::fake_timer();
    loader.set_timer(Box::new(timer));

    assert_eq!(handle.delays(), vec![80], "default loader interval");
    let before = renders.get();
    assert!(handle.fire());
    assert_eq!(renders.get(), before + 1);
    assert!(handle.is_pending(), "re-armed");
}

#[test]
fn renders_within_every_width() {
    let _guard = common::theme_lock();
    common::setup();
    for cancellable in [true, false] {
        let mut loader = BorderedLoader::new(
            None,
            common::current_theme(),
            "A rather long loading message that will wrap",
            Some(BorderedLoaderOptions { cancellable }),
        );
        common::assert_width_invariant(&mut loader, &common::WIDTHS);
    }
}

#[test]
fn border_uses_the_theme_border_token() {
    let _guard = common::theme_lock();
    common::setup();
    let mut loader = BorderedLoader::new(None, common::current_theme(), "M", None);
    let lines = loader.render(10);
    assert!(lines[0].starts_with(&common::fg_ansi(ThemeColor::Border)));
    assert!(lines[lines.len() - 1].starts_with(&common::fg_ansi(ThemeColor::Border)));
}

/// 回归：TS 的所有仓库内调用点传的是全局 `theme` Proxy（`theme.fg()` 每次重新解析当前
/// 主题），因此在 loader 挂载期间切换主题时边框颜色必须跟着变。
#[test]
fn follows_theme_switches_when_constructed_with_the_global_theme() {
    let _guard = common::theme_lock();
    common::setup();
    kiana_theme::init_theme(Some("dark"), false);

    let mut loader = BorderedLoader::new(None, common::current_theme(), "M", None);
    let dark = loader.render(10);
    let dark_border = common::fg_ansi(ThemeColor::Border);
    assert!(dark[0].starts_with(&dark_border), "dark: {:?}", dark[0]);

    kiana_theme::init_theme(Some("light"), false);
    let light = loader.render(10);
    let light_border = common::fg_ansi(ThemeColor::Border);
    assert_ne!(dark_border, light_border, "builtin themes differ");
    assert!(
        light[0].starts_with(&light_border),
        "border must follow the global theme switch (TS Proxy): {:?}",
        light[0]
    );
    assert_ne!(dark[0], light[0]);

    kiana_theme::init_theme(Some("dark"), false);
}

/// 显式传入另一个主题实例（不是全局主题）时保持快照语义（spec §2.12）。
#[test]
fn keeps_an_explicit_theme_instance_as_a_snapshot() {
    let _guard = common::theme_lock();
    common::setup();
    kiana_theme::init_theme(Some("light"), false);

    let snapshot = kiana_theme::get_theme_by_name("dark").expect("builtin dark theme");
    let expected = snapshot.fg(ThemeColor::Border, &"─".repeat(10));
    let mut loader = BorderedLoader::new(None, snapshot, "M", None);

    // 全局切到 dark 再把全局切到 light：快照不能被全局状态影响。
    kiana_theme::init_theme(Some("dark"), false);
    kiana_theme::init_theme(Some("light"), false);
    let lines = loader.render(10);
    assert_eq!(lines[0], expected);
    assert_eq!(lines[lines.len() - 1], expected);

    kiana_theme::init_theme(Some("dark"), false);
}

#[test]
fn empty_message_renders_safely_at_every_width() {
    let _guard = common::theme_lock();
    common::setup();
    for cancellable in [true, false] {
        let mut loader = BorderedLoader::new(
            None,
            common::current_theme(),
            "",
            Some(BorderedLoaderOptions { cancellable }),
        );
        // 空 message 的 loader 仍然输出消息行（空白）；80 列下提示不换行，高度与常规一致。
        assert_eq!(loader.render(80).len(), if cancellable { 7 } else { 5 });
        common::assert_width_invariant(&mut loader, &common::WIDTHS);
        loader.dispose();
        loader.dispose();
    }
}
