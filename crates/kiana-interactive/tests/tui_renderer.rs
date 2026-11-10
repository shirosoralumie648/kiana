//! `suite/regressions/7731-tui-method-wrapping.test.ts` 的移植。
//!
//! TS 的 Proxy 带方法缓存失效检查；Rust 的代理在每次调用时解析当前渲染器，因此"取出的
//! 方法"（[`InteractiveTuiReference::capture_render`] 等）始终路由到当前渲染器。
//!
//! 与 TS 的差异：Rust 没有 `tui.render = fn` 这样的动态属性赋值（Proxy `set` 陷阱），
//! 用例 1 用"包裹先前取出的句柄的闭包"表达同一语义。

use std::cell::RefCell;
use std::rc::Rc;

use kiana_interactive::tui_renderer::{
    EditorSlot, InteractiveRenderer, InteractiveTuiOptions, create_bordered_loader,
    create_interactive_tui, create_interactive_tui_reference, restore_editor_with_loader,
};
use kiana_tui::alt_screen::TuiAltScreenOptions;
use kiana_tui::components::text_component::Text;
use kiana_tui::tui::{Container, Tui, TuiMode, TuiStopOptions};

use crate::common::{TestTerminal, alt_screen, main_screen, shared};

mod common;

fn options(tui_mode: TuiMode) -> InteractiveTuiOptions {
    InteractiveTuiOptions {
        tui_mode,
        show_hardware_cursor: false,
        log_directory: std::env::temp_dir().to_string_lossy().into_owned(),
        terminal: Some(Box::new(TestTerminal::new())),
        on_right_click_paste: None,
        fullscreen_copy_on_select: None,
        fullscreen_wheel_scroll_lines: None,
    }
}

fn rendered_text(lines: &[String]) -> String {
    kiana_tui::text::strip_terminal_sequences(&lines.join("\n"))
}

#[test]
fn calls_the_method_captured_before_a_replacement() {
    let renderer = create_interactive_tui(options(TuiMode::Regular));
    renderer
        .borrow_mut()
        .add_child(Box::new(Text::new("width: rendered")));
    let mut reference = create_interactive_tui_reference(Rc::clone(&renderer));

    // TS: `const originalRender = tui.render; tui.render = (w) => originalRender(w);`
    let original_render = reference.capture_render();
    let wrapper = {
        let original_render = Rc::clone(&original_render);
        move |width: usize| original_render(width)
    };

    let lines = wrapper(80);
    assert!(
        rendered_text(&lines).contains("width: rendered"),
        "rendered: {:?}",
        rendered_text(&lines)
    );
    assert_eq!(reference.render(80), lines);
}

#[test]
fn routes_a_captured_method_to_a_replacement_renderer() {
    let renderer = create_interactive_tui(options(TuiMode::Regular));
    let reference = create_interactive_tui_reference(Rc::clone(&renderer));

    renderer
        .borrow_mut()
        .add_child(Box::new(Text::new("regular renderer")));
    let regular_requested = renderer.borrow().render_requested_handle();
    let captured_render = reference.capture_render();
    let captured_request_render = reference.capture_request_render();

    let lines = captured_render(80);
    assert!(rendered_text(&lines).contains("regular renderer"));
    captured_request_render(false);
    assert!(
        regular_requested.get(),
        "regular renderer must be asked to render"
    );

    // 切换渲染器（TS `this.renderer = nextUi`）。
    regular_requested.set(false);
    let mut fullscreen = alt_screen(TestTerminal::new(), TuiAltScreenOptions::default());
    fullscreen.add_child(Box::new(Text::new("fullscreen renderer")));
    let fullscreen_requested = fullscreen.render_requested_handle();
    *renderer.borrow_mut() = InteractiveRenderer::Fullscreen(fullscreen);

    let lines = captured_render(80);
    assert!(
        rendered_text(&lines).contains("fullscreen renderer"),
        "rendered: {:?}",
        rendered_text(&lines)
    );
    captured_request_render(false);
    assert!(
        fullscreen_requested.get(),
        "fullscreen renderer must be asked to render"
    );
    assert!(
        !regular_requested.get(),
        "the replaced renderer must not receive the call"
    );
}

#[test]
fn dispatches_mode_through_the_current_renderer() {
    let renderer = create_interactive_tui(options(TuiMode::Regular));
    let reference = create_interactive_tui_reference(Rc::clone(&renderer));
    assert_eq!(reference.mode(), TuiMode::Regular);

    let fullscreen = create_interactive_tui(options(TuiMode::Fullscreen));
    assert_eq!(fullscreen.borrow().mode(), TuiMode::Fullscreen);
    assert!(fullscreen.borrow().is_viewport_tui());
    assert!(!renderer.borrow().is_viewport_tui());

    // 主屏的 captureRenderState 可用，全屏没有。
    assert!(reference.capture_render_state().is_some());
    assert!(fullscreen.borrow().capture_render_state().is_none());

    // `main_screen` 便捷构造函数与 createInteractiveTui 一致。
    let direct = main_screen(TestTerminal::new());
    assert_eq!(direct.mode(), TuiMode::Regular);
}

/// `interactive-tui.test.ts` 的 `selects the alternate-screen renderer only when requested`。
///
/// TS 断言 regular 模式启动后**没有**写出 `\x1b[?1049h`，fullscreen 模式启动后写出。
/// Rust 的进入序列在 `TuiAltScreen::beforeTerminalStart`（`start()` 内）写一次。
#[test]
fn enters_the_alternate_screen_only_in_fullscreen_mode() {
    let regular_terminal = TestTerminal::new();
    let regular_output = regular_terminal.output_handle();
    let regular = create_interactive_tui(InteractiveTuiOptions {
        terminal: Some(Box::new(regular_terminal)),
        ..options(TuiMode::Regular)
    });
    regular.borrow_mut().start();
    assert!(
        !regular_output.borrow().contains("\x1b[?1049h"),
        "regular mode must not enter the alternate screen"
    );
    regular.borrow_mut().stop(TuiStopOptions {
        preserve_screen: true,
    });

    let alt_terminal = TestTerminal::new();
    let alt_output = alt_terminal.output_handle();
    let alt = create_interactive_tui(InteractiveTuiOptions {
        terminal: Some(Box::new(alt_terminal)),
        ..options(TuiMode::Fullscreen)
    });
    alt.borrow_mut().start();
    assert!(
        alt_output.borrow().contains("\x1b[?1049h"),
        "fullscreen mode must enter the alternate screen"
    );
    alt.borrow_mut().stop(TuiStopOptions {
        preserve_screen: true,
    });
}

fn editor_slot(ui: Rc<RefCell<dyn Tui>>) -> EditorSlot {
    EditorSlot::new(
        ui,
        Rc::new(RefCell::new(Container::new())),
        shared(Text::new("editor")),
    )
}

/// `session-share.ts` 的 `restoreEditor` 没有 `requestRender()`：恢复编辑器本身不得请求
/// 渲染（`bug-report.ts` 的版本才有，见下一个用例）。
#[test]
fn restoring_the_editor_does_not_request_a_render() {
    let screen = main_screen(TestTerminal::new());
    let requested = screen.base().render_requested_handle();
    let ui: Rc<RefCell<dyn Tui>> = Rc::new(RefCell::new(screen));

    let slot = editor_slot(Rc::clone(&ui));
    slot.show(Box::new(Text::new("overlay")));
    assert!(requested.get(), "showOverlay requests a render");

    requested.set(false);
    slot.restore();
    assert!(
        !requested.get(),
        "session-share restoreEditor must not request a render"
    );
}

/// `bug-report.ts` 的 `restoreEditor` 末尾有 `requestRender()`；
/// `restore_editor_with_loader` 是它的 Rust 等价物。
#[test]
fn restoring_with_a_bug_report_loader_requests_a_render() {
    kiana_theme::init_theme(Some("dark"), false);
    let screen = main_screen(TestTerminal::new());
    let requested = screen.base().render_requested_handle();
    let ui: Rc<RefCell<dyn Tui>> = Rc::new(RefCell::new(screen));

    let slot = editor_slot(Rc::clone(&ui));
    let loader = create_bordered_loader(&ui, "Uploading bug report...");
    slot.show(Box::new(
        kiana_interactive::tui_renderer::SharedLoader::new(Rc::clone(&loader)),
    ));

    requested.set(false);
    restore_editor_with_loader(&slot, &loader);
    assert!(
        requested.get(),
        "bug-report restoreEditor requests a render"
    );
}
