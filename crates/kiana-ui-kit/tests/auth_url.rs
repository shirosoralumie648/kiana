//! `AuthUrlComponent` 的组件级测试（对应 TS `auth-url-copy.test.ts` 的相关断言）。

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use kiana_theme::ThemeColor;
use kiana_tui::Component;
use kiana_ui_kit::AuthUrlComponent;

const URL: &str = "https://auth.example.invalid/authorize?xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";

fn click_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "Cmd+click to open"
    } else {
        "Ctrl+click to open"
    }
}

#[test]
fn renders_url_hyperlink_and_key_hint() {
    common::setup();
    let (request_render, renders) = common::request_render();
    let mut component = AuthUrlComponent::new(Some(request_render), URL);

    assert_eq!(component.url, URL);

    // 构造时会调用 setHint → requestRender。
    assert_eq!(renders.get(), 1);

    let plain = common::render_plain(&mut component, 80);
    assert!(plain.contains("ctrl+x to copy"), "plain: {plain:?}");
    assert!(plain.contains(click_hint()), "plain: {plain:?}");
    assert!(plain.contains("•"), "plain: {plain:?}");
    // 长 URL 在 80 列会换行；在足够宽的渲染里应完整出现。
    let wide = kiana_tui::strip_terminal_sequences(&component.render(URL.len() + 10).join(""));
    assert!(wide.contains(URL), "wide: {wide:?}");

    // OSC 8 超链接：ST（ESC + 反斜杠）结尾，text 在前。
    let url_line = component.render(URL.len() + 10)[0].clone();
    let expected_link = format!("\x1b]8;;{URL}\x1b\\{URL}\x1b]8;;\x1b\\");
    assert!(url_line.contains(&expected_link), "url line: {url_line:?}");
    let hint = component.hint_text();
    let expected_click_link = format!("\x1b]8;;{URL}\x1b\\{}\x1b]8;;\x1b\\", click_hint());
    assert!(hint.contains(&expected_click_link), "hint: {hint:?}");

    // URL 用 accent 色（左留白 1 列，所以不在行首）。
    assert!(
        url_line.contains(&common::fg_ansi(ThemeColor::Accent)),
        "url line should carry accent ansi: {url_line:?}"
    );
}

#[test]
fn copy_success_shows_copied_hint() {
    common::setup();
    let (request_render, renders) = common::request_render();
    let mut component = AuthUrlComponent::new(Some(request_render), URL);
    let renders_before = renders.get();

    let received: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&received);
    common::block_on(component.copy_with(|url: String| async move {
        sink.borrow_mut().push(url);
        Ok::<(), String>(())
    }));

    assert_eq!(*received.borrow(), vec![URL.to_string()]);
    assert_eq!(
        component.hint_text(),
        {
            // 点击提示 + 分隔符保持 dim，suffix 由调用方着色（success）。
            let mut expected = common::fg(
                ThemeColor::Dim,
                &format!("\x1b]8;;{URL}\x1b\\{}\x1b]8;;\x1b\\", click_hint()),
            );
            expected.push(' ');
            expected.push_str(&common::fg(ThemeColor::Dim, "•"));
            expected.push(' ');
            expected.push_str(&common::fg(ThemeColor::Success, "Copied URL to clipboard"));
            expected
        },
        "suffix must not be re-colored, only wrapped in success"
    );
    assert_eq!(renders.get(), renders_before + 1);

    let plain = common::render_plain(&mut component, 80);
    assert!(plain.contains("Copied URL to clipboard"));
}

#[test]
fn copy_failure_shows_error_message_verbatim() {
    common::setup();
    let (request_render, renders) = common::request_render();
    let mut component = AuthUrlComponent::new(Some(request_render), URL);
    let renders_before = renders.get();

    common::block_on(component.copy_with(|_url: String| async move {
        Err::<(), String>("Clipboard unavailable: no clipboard tool".to_string())
    }));

    assert!(component.hint_text().contains(&common::fg(
        ThemeColor::Error,
        "Clipboard unavailable: no clipboard tool"
    )));
    assert_eq!(renders.get(), renders_before + 1);
    let plain = common::render_plain(&mut component, 80);
    assert!(plain.contains("Clipboard unavailable: no clipboard tool"));
}

#[test]
fn renders_within_every_width() {
    common::setup();
    let (request_render, _renders) = common::request_render();
    let mut component = AuthUrlComponent::new(Some(request_render), URL);
    common::assert_width_invariant(&mut component, &common::WIDTHS);

    let mut empty = AuthUrlComponent::new(None, "");
    common::assert_width_invariant(&mut empty, &common::WIDTHS);
}

#[test]
fn invalidate_and_mouse_delegate_without_panicking() {
    common::setup();
    let mut component = AuthUrlComponent::new(None, URL);
    component.invalidate();
    let before = component.render(40);
    component.invalidate();
    assert_eq!(component.render(40), before);
    assert!(!kiana_tui::Component::handles_input(&component));
    assert_eq!(
        component
            .container_children()
            .map(|children| children.len()),
        Some(2)
    );
}

#[test]
fn hint_with_unbound_copy_key_keeps_the_leading_space() {
    common::setup();
    // 只保留 tui 默认定义（没有 app.message.copy）→ keyText 为空串，但提示仍输出描述前的空格。
    kiana_tui::set_keybindings(kiana_tui::keybindings::KeybindingsManager::with_defaults());
    let component = AuthUrlComponent::new(None, URL);
    let hint = component.hint_text();
    assert!(
        hint.contains(&common::fg(ThemeColor::Muted, " to copy")),
        "hint: {hint:?}"
    );
    // 还原本线程的测试键位表。
    common::install_keybindings();
}
