//! `extension-selector` / `extension-input` / `extension-editor` 的行为测试。
//!
//! TS 侧没有直接覆盖这三个组件的测试文件；这里覆盖 spec/ca-interactive-components.md
//! §2.54.3–§2.54.5、§2.54.10 的关键行为（clamp、展开键、提交/取消、倒计时标题、外部编辑器）。

use std::cell::{Cell, RefCell};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::OnceLock;

use kiana_config::keybindings::{KeybindingsConfig, KeybindingsManager as AppKeybindingsManager};
use kiana_core::ansi::strip_ansi;
use kiana_interactive::components::extension_editor::{
    EditorSpawn, ExtensionEditorComponent, ExtensionEditorOptions,
};
use kiana_interactive::components::extension_input::{
    ExtensionInputComponent, ExtensionInputOptions,
};
use kiana_interactive::components::extension_selector::{
    ExtensionSelectorComponent, ExtensionSelectorOptions,
};
use kiana_theme::init_theme;
use kiana_tui::components::editor::EditorTui;
use kiana_tui::keybindings::{
    KeybindingsConfig as TuiKeybindingsConfig, KeybindingsManager, set_keybindings,
};
use kiana_tui::terminal_colors::{ProgramStatus, Terminal, TerminalColors};
use kiana_tui::tui::{
    Component, FocusTarget, Focusable, InputListenerId, OverlayBounds, OverlayHandle,
    OverlayOptions, OverlayUnfocusOptions, Tui, TuiInputListener, TuiMode, TuiMouseButton,
    TuiMouseEvent, TuiMouseEventType, TuiStopOptions,
};

fn theme() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| init_theme(Some("dark"), false));
}

fn install_keybindings() {
    set_keybindings(KeybindingsManager::new(
        kiana_config::keybindings::keybindings().clone(),
        TuiKeybindingsConfig::new(),
    ));
}

fn strip_apc_sequences(text: &str) -> String {
    let mut output = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("\x1b_") {
        output.push_str(&rest[..start]);
        rest = &rest[start..];
        match rest.find('\x07') {
            Some(end) => rest = &rest[end + 1..],
            None => return output,
        }
    }
    output.push_str(rest);
    output
}

/// 去掉 ANSI 与零宽光标标记（Input/Editor 的 APC fake-cursor）。
fn rendered(component: &mut dyn Component, width: usize) -> String {
    strip_apc_sequences(&strip_ansi(&component.render(width).join("\n")))
}

fn app_keybindings() -> Rc<AppKeybindingsManager> {
    Rc::new(AppKeybindingsManager::new(KeybindingsConfig::new(), None))
}

/// 与 TS `TuiMouseEvent` 同形的鼠标事件（`x`/`y` 是相对接收组件的坐标）。
fn mouse_event(
    event_type: TuiMouseEventType,
    x: isize,
    y: isize,
    width: usize,
    height: usize,
) -> TuiMouseEvent {
    TuiMouseEvent {
        event_type,
        button: TuiMouseButton::Left,
        x,
        y,
        screen_x: x,
        screen_y: y,
        width,
        height,
        shift: false,
        alt: false,
        ctrl: false,
        wheel_delta: None,
        click_count: (event_type == TuiMouseEventType::Click).then_some(1),
    }
}

// ---------------------------------------------------------------------------
// fake TUI / terminal（本测试文件专用；本单元先于 support-subsystems 合并，不能用
// tests/common/mod.rs）
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FakeTui {
    children: Vec<Box<dyn Component>>,
    focus: Option<FocusTarget>,
    stops: Rc<Cell<usize>>,
    starts: Rc<Cell<usize>>,
    forced_renders: Rc<Cell<usize>>,
    renders: Rc<Cell<usize>>,
}

impl Component for FakeTui {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.renders.set(self.renders.get() + 1);
        let mut lines = Vec::new();
        for child in &mut self.children {
            lines.extend(child.render(width));
        }
        lines
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        Some(&self.children)
    }

    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        Some(&mut self.children)
    }
}

impl Tui for FakeTui {
    fn mode(&self) -> TuiMode {
        TuiMode::Regular
    }

    fn children(&mut self) -> &mut Vec<Box<dyn Component>> {
        &mut self.children
    }

    fn add_child(&mut self, child: Box<dyn Component>) {
        self.children.push(child);
    }

    fn remove_child(&mut self, child: &dyn Component) {
        let target = child as *const dyn Component as *const ();
        if let Some(index) = self
            .children
            .iter()
            .position(|candidate| candidate.as_ref() as *const dyn Component as *const () == target)
        {
            self.children.remove(index);
        }
    }

    fn clear_children(&mut self) {
        self.children.clear();
    }

    fn focus(&self) -> Option<FocusTarget> {
        self.focus
    }

    fn set_focus(&mut self, target: Option<FocusTarget>) {
        self.focus = target;
    }

    fn show_overlay(
        &mut self,
        _component: Box<dyn Component>,
        _options: OverlayOptions,
    ) -> Box<dyn OverlayHandle> {
        Box::new(StubOverlayHandle)
    }

    fn hide_overlay(&mut self) {}

    fn has_overlay(&self) -> bool {
        false
    }

    fn start(&mut self) {
        self.starts.set(self.starts.get() + 1);
    }

    fn stop(&mut self, _options: TuiStopOptions) {
        self.stops.set(self.stops.get() + 1);
    }

    fn render_now(&mut self, _force: bool) {
        self.renders.set(self.renders.get() + 1);
    }

    fn request_render(&mut self, force: bool) {
        if force {
            self.forced_renders.set(self.forced_renders.get() + 1);
        }
        self.renders.set(self.renders.get() + 1);
    }

    fn add_input_listener(&mut self, _listener: TuiInputListener) -> InputListenerId {
        InputListenerId(0)
    }

    fn remove_input_listener(&mut self, _id: InputListenerId) {}

    fn query_terminal_colors(
        &mut self,
        _timeout_ms: u64,
    ) -> Pin<Box<dyn std::future::Future<Output = TerminalColors> + '_>> {
        Box::pin(async { TerminalColors::default() })
    }
}

struct StubOverlayHandle;

impl OverlayHandle for StubOverlayHandle {
    fn hide(&mut self) {}
    fn set_hidden(&mut self, _hidden: bool) {}
    fn is_hidden(&self) -> bool {
        false
    }
    fn focus(&mut self) {}
    fn unfocus(&mut self, _options: Option<OverlayUnfocusOptions>) {}
    fn is_focused(&self) -> bool {
        false
    }
    fn bounds(&self) -> Option<OverlayBounds> {
        None
    }
}

#[derive(Default)]
struct FakeTerminal {
    rows: usize,
    writes: RefCell<String>,
}

impl Terminal for FakeTerminal {
    fn start(&mut self, _on_input: Box<dyn FnMut(&str)>, _on_resize: Box<dyn FnMut()>) {}
    fn stop(&mut self) {}
    fn drain_input(
        &mut self,
        _max_ms: u64,
        _idle_ms: u64,
    ) -> Pin<Box<dyn std::future::Future<Output = ()> + '_>> {
        Box::pin(async {})
    }
    fn write(&mut self, data: &str) {
        self.writes.borrow_mut().push_str(data);
    }
    fn columns(&self) -> usize {
        80
    }
    fn rows(&self) -> usize {
        if self.rows == 0 { 24 } else { self.rows }
    }
    fn kitty_protocol_active(&self) -> bool {
        false
    }
    fn move_by(&mut self, _lines: isize) {}
    fn hide_cursor(&mut self) {}
    fn show_cursor(&mut self) {}
    fn clear_line(&mut self) {}
    fn clear_from_cursor(&mut self) {}
    fn clear_screen(&mut self) {}
    fn set_title(&mut self, _title: &str) {}
    fn set_progress(&mut self, _active: bool) {}
    fn set_program_status(&mut self, _status: &ProgramStatus) {}
}

struct TuiHarness {
    tui: Rc<RefCell<dyn Tui>>,
    stops: Rc<Cell<usize>>,
    starts: Rc<Cell<usize>>,
    forced_renders: Rc<Cell<usize>>,
}

type PendingEditorTask = Rc<RefCell<Option<Pin<Box<dyn std::future::Future<Output = ()>>>>>>;

fn fake_tui() -> TuiHarness {
    let stops = Rc::new(Cell::new(0));
    let starts = Rc::new(Cell::new(0));
    let forced_renders = Rc::new(Cell::new(0));
    let c = Rc::clone(&stops);
    let s = Rc::clone(&starts);
    let f = Rc::clone(&forced_renders);
    let tui: Rc<RefCell<dyn Tui>> = Rc::new(RefCell::new(FakeTui {
        stops: c,
        starts: s,
        forced_renders: f,
        ..FakeTui::default()
    }));
    TuiHarness {
        tui,
        stops,
        starts,
        forced_renders,
    }
}

fn editor_tui() -> EditorTui {
    let terminal: Rc<RefCell<Box<dyn Terminal>>> =
        Rc::new(RefCell::new(Box::new(FakeTerminal::default())));
    EditorTui::new(terminal, Rc::new(|| {}))
}

// ---------------------------------------------------------------------------
// extension-selector.ts
// ---------------------------------------------------------------------------

#[test]
fn extension_selector_clamps_and_never_wraps() {
    theme();
    install_keybindings();
    let selected: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let selected_log = Rc::clone(&selected);
    let cancels = Rc::new(Cell::new(0));
    let cancel_log = Rc::clone(&cancels);
    let toggles = Rc::new(Cell::new(0));
    let toggle_log = Rc::clone(&toggles);

    let mut selector = ExtensionSelectorComponent::new(
        "Pick",
        vec!["a".to_string(), "b".to_string(), "c".to_string()],
        move |option| selected_log.borrow_mut().push(option.to_string()),
        move || cancel_log.set(cancel_log.get() + 1),
        ExtensionSelectorOptions {
            on_toggle_tools_expanded: Some(Box::new(move || toggle_log.set(toggle_log.get() + 1))),
            ..ExtensionSelectorOptions::default()
        },
    );

    // up 在 0 处 clamp。
    selector.handle_input("\x1b[A");
    assert_eq!(selector.selected_index(), 0);
    selector.handle_input("\x1b[B");
    selector.handle_input("\x1b[B");
    assert_eq!(selector.selected_index(), 2);
    selector.handle_input("\x1b[B");
    assert_eq!(
        selector.selected_index(),
        2,
        "down clamps at the last option"
    );

    // app.tools.expand 先处理，不影响选中。
    selector.handle_input("\x0f");
    assert_eq!(toggles.get(), 1);
    assert_eq!(selector.selected_index(), 2);

    selector.handle_input("k");
    assert_eq!(selector.selected_index(), 1);
    selector.handle_input("j");
    assert_eq!(selector.selected_index(), 2);

    selector.handle_input("\r");
    assert_eq!(selected.borrow().clone(), vec!["c".to_string()]);
    selector.handle_input("\x1b");
    assert_eq!(cancels.get(), 1);

    // dispose 幂等。
    selector.dispose();
    selector.dispose();
}

#[test]
fn extension_selector_ignores_confirm_on_an_empty_option() {
    theme();
    install_keybindings();
    let selected: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let selected_log = Rc::clone(&selected);
    let mut selector = ExtensionSelectorComponent::new(
        "Pick",
        vec![String::new(), "b".to_string()],
        move |option| selected_log.borrow_mut().push(option.to_string()),
        || {},
        ExtensionSelectorOptions::default(),
    );

    // TS `const sel = options[selectedIndex]; if (sel) onSelect(sel)`：空串为假。
    selector.handle_input("\r");
    assert!(
        selected.borrow().is_empty(),
        "confirming an empty-string option must not invoke onSelect"
    );

    selector.handle_input("\x1b[B");
    selector.handle_input("\r");
    assert_eq!(selected.borrow().clone(), vec!["b".to_string()]);
}

#[test]
fn extension_selector_countdown_updates_the_title() {
    theme();
    install_keybindings();
    let mut selector = ExtensionSelectorComponent::new(
        "Pick",
        vec!["a".to_string()],
        |_| {},
        || {},
        ExtensionSelectorOptions {
            tui: Some(Rc::new(|| {})),
            timeout: Some(5000.0),
            ..ExtensionSelectorOptions::default()
        },
    );
    assert!(selector.title_text().contains("Pick (5s)"));
    selector.dispose();
}

// ---------------------------------------------------------------------------
// extension-input.ts
// ---------------------------------------------------------------------------

#[test]
fn extension_input_submits_initial_value_plus_typed_text() {
    theme();
    install_keybindings();
    let submitted: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let submitted_log = Rc::clone(&submitted);
    let cancels = Rc::new(Cell::new(0));
    let cancel_log = Rc::clone(&cancels);

    let mut input = ExtensionInputComponent::new(
        "Input",
        Some("ignored placeholder"),
        move |value| *submitted_log.borrow_mut() = Some(value.to_string()),
        move || cancel_log.set(cancel_log.get() + 1),
        ExtensionInputOptions {
            initial_value: Some("hello".to_string()),
            description: Some("Describe it".to_string()),
            ..ExtensionInputOptions::default()
        },
    );

    let output = rendered(&mut input, 80);
    assert!(output.contains("Input"));
    assert!(output.contains("Describe it"));
    assert!(output.contains("enter submit"));
    assert!(output.contains("escape/ctrl+c cancel"));
    assert!(output.contains("> hello"));

    input.handle_input(" world");
    input.handle_input("\r");
    // TS `Input.setValue` 只把 cursor 夹到 `[0, len]`（初值 0），因此后续输入插在开头。
    assert_eq!(submitted.borrow().clone(), Some(" worldhello".to_string()));

    input.handle_input("\x1b");
    assert_eq!(cancels.get(), 1);
    input.dispose();
    input.dispose();
}

#[test]
fn extension_input_timeout_updates_the_title() {
    theme();
    install_keybindings();
    let mut input = ExtensionInputComponent::new(
        "Prompt",
        None,
        |_| {},
        || {},
        ExtensionInputOptions {
            tui: Some(Rc::new(|| {})),
            timeout: Some(3000.0),
            ..ExtensionInputOptions::default()
        },
    );
    assert!(input.title_text().contains("Prompt (3s)"));
    input.dispose();
}

#[test]
fn extension_input_focus_propagates_to_the_input() {
    theme();
    install_keybindings();
    let mut input = ExtensionInputComponent::new(
        "Prompt",
        None,
        |_| {},
        || {},
        ExtensionInputOptions::default(),
    );
    assert!(!input.focused());
    input.set_focused(true);
    assert!(input.focused());
}

/// 回归（minor）：TS `ExtensionInputComponent extends Container`，点击输入框会定位光标。
#[test]
fn extension_input_forwards_clicks_to_the_input() {
    theme();
    install_keybindings();
    let submitted: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let submitted_log = Rc::clone(&submitted);
    let mut input = ExtensionInputComponent::new(
        "Input",
        None,
        move |value| *submitted_log.borrow_mut() = Some(value.to_string()),
        || {},
        ExtensionInputOptions {
            initial_value: Some("hello".to_string()),
            ..ExtensionInputOptions::default()
        },
    );
    let lines = input.render(80);
    let height = lines.len();
    let row = lines
        .iter()
        .position(|line| kiana_tui::strip_terminal_sequences(line).contains("> hello"))
        .expect("input row");
    let press = input
        .handle_mouse(&mouse_event(
            TuiMouseEventType::Press,
            4,
            row as isize,
            80,
            height,
        ))
        .expect("press must reach the Input");
    assert_eq!(press.handled, Some(true));
    assert_eq!(press.focus, Some(true));

    input.handle_input("X");
    input.handle_input("\r");
    assert_eq!(submitted.borrow().clone(), Some("heXllo".to_string()));
}

// ---------------------------------------------------------------------------
// extension-editor.ts
// ---------------------------------------------------------------------------

fn extension_editor(
    harness: &TuiHarness,
    external_editor_command: Option<String>,
    on_submit: impl FnMut(&str) + 'static,
    on_cancel: impl FnMut() + 'static,
) -> ExtensionEditorComponent {
    ExtensionEditorComponent::new(
        Rc::clone(&harness.tui),
        editor_tui(),
        app_keybindings(),
        "Editor",
        Some("prefill"),
        on_submit,
        on_cancel,
        ExtensionEditorOptions {
            description: Some("Extension editor".to_string()),
            ..ExtensionEditorOptions::default()
        },
        external_editor_command,
    )
}

#[test]
fn extension_editor_renders_and_submits() {
    theme();
    install_keybindings();
    let harness = fake_tui();
    let submitted: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let submitted_log = Rc::clone(&submitted);
    let cancels = Rc::new(Cell::new(0));
    let cancel_log = Rc::clone(&cancels);
    let mut editor = extension_editor(
        &harness,
        Some("my-editor".to_string()),
        move |value| *submitted_log.borrow_mut() = Some(value.to_string()),
        move || cancel_log.set(cancel_log.get() + 1),
    );

    assert_eq!(editor.external_editor_command(), "my-editor");
    assert_eq!(editor.editor_text(), "prefill");
    let output = rendered(&mut editor, 120);
    assert!(output.contains("Editor"));
    assert!(output.contains("Extension editor"));
    assert!(output.contains("enter submit"));
    assert!(output.contains("shift+enter/ctrl+j newline"));
    assert!(output.contains("escape/ctrl+c cancel"));
    assert!(output.contains("ctrl+g external editor"));

    editor.handle_input(" more");
    editor.handle_input("\r");
    assert_eq!(submitted.borrow().clone(), Some("prefill more".to_string()));

    editor.handle_input("\x1b");
    assert_eq!(cancels.get(), 1);
}

#[test]
fn extension_editor_focus_propagates_to_the_inner_editor() {
    theme();
    install_keybindings();
    let harness = fake_tui();
    let mut editor = extension_editor(&harness, Some("my-editor".to_string()), |_| {}, || {});
    assert!(!editor.focused());
    editor.set_focused(true);
    assert!(editor.focused());
}

/// 回归（minor）：TS `ExtensionEditorComponent extends Container`，点击编辑区由
/// `Editor.handleMouse` 定位光标（Rust 之前 `handle_mouse` 是默认 None）。
#[test]
fn extension_editor_forwards_clicks_to_the_editor() {
    theme();
    install_keybindings();
    let harness = fake_tui();
    let mut editor = extension_editor(&harness, Some("my-editor".to_string()), |_| {}, || {});
    let lines = editor.render(120);
    let height = lines.len();
    let row = lines
        .iter()
        .position(|line| strip_apc_sequences(&strip_ansi(line)).contains("prefill"))
        .expect("editor row");
    let click = editor
        .handle_mouse(&mouse_event(
            TuiMouseEventType::Click,
            3,
            row as isize,
            120,
            height,
        ))
        .expect("click must reach the Editor");
    assert_eq!(click.handled, Some(true));
    assert_eq!(click.focus, Some(true));

    // x=3 → 列 3 → 光标位于 "pre|fill"；插入字符后应落在中间。
    editor.handle_input("X");
    assert_eq!(editor.editor_text(), "preXfill");
}

#[test]
fn extension_editor_ctrl_g_runs_the_external_editor_between_stop_and_start() {
    theme();
    install_keybindings();
    let harness = fake_tui();
    let submitted: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let submitted_log = Rc::clone(&submitted);

    let test_directory = tempfile::Builder::new()
        .prefix("kiana-editor-test-")
        .tempdir()
        .expect("temp dir");
    let capture_path = test_directory.path().join("capture.json");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fake-external-editor.mjs");
    let node = std::env::var("NODE").unwrap_or_else(|_| "node".to_string());
    let command = format!("{} {} {}", node, fixture.display(), capture_path.display());

    let mut editor = extension_editor(
        &harness,
        Some(command),
        move |value| submitted_log.borrow_mut().push(value.to_string()),
        || {},
    );
    assert_eq!(editor.editor_text(), "prefill");

    let pending: PendingEditorTask = Rc::new(RefCell::new(None));
    let pending_log = Rc::clone(&pending);
    let spawn: EditorSpawn = Rc::new(move |future| {
        *pending_log.borrow_mut() = Some(future);
    });
    editor.set_spawn(spawn);

    // ctrl+g：TS 在第一个 await 之前同步 `tui.stop()`，之后才投递异步任务（不阻塞 UI）。
    editor.handle_input("\x07");
    assert_eq!(
        harness.stops.get(),
        1,
        "tui.stop() must happen synchronously on the external-editor key"
    );
    assert!(pending.borrow().is_some());

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let future = pending
            .borrow_mut()
            .take()
            .expect("pending external editor task");
        future.await;
    });

    assert_eq!(harness.stops.get(), 1);
    assert_eq!(harness.starts.get(), 1);
    assert_eq!(harness.forced_renders.get(), 1);
    assert_eq!(editor.editor_text(), "edited");
    let capture = std::fs::read_to_string(&capture_path).expect("capture");
    let capture: serde_json::Value = serde_json::from_str(&capture).expect("capture json");
    assert_eq!(capture["content"], "prefill");
}

#[test]
fn extension_editor_external_failure_keeps_the_text_and_still_restarts_the_tui() {
    theme();
    install_keybindings();
    let harness = fake_tui();

    let test_directory = tempfile::Builder::new()
        .prefix("kiana-editor-test-")
        .tempdir()
        .expect("temp dir");
    let capture_path = test_directory.path().join("capture.json");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fake-external-editor.mjs");
    let node = std::env::var("NODE").unwrap_or_else(|_| "node".to_string());
    let command = format!(
        "{} {} {} --fail",
        node,
        fixture.display(),
        capture_path.display()
    );

    let mut editor = extension_editor(&harness, Some(command), |_| {}, || {});
    let pending: PendingEditorTask = Rc::new(RefCell::new(None));
    let pending_log = Rc::clone(&pending);
    editor.set_spawn(Rc::new(move |future| {
        *pending_log.borrow_mut() = Some(future);
    }));

    editor.handle_input("\x07");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let future = pending
            .borrow_mut()
            .take()
            .expect("pending external editor task");
        future.await;
    });

    assert_eq!(
        editor.editor_text(),
        "prefill",
        "failed editor keeps content"
    );
    assert_eq!(harness.stops.get(), 1);
    assert_eq!(harness.starts.get(), 1);
    assert_eq!(harness.forced_renders.get(), 1);
}

// ---------------------------------------------------------------------------
// 渲染 golden（由真实 TS 组件在 dark 主题、80 列下生成）
// ---------------------------------------------------------------------------

fn component_golden(key: &str) -> Vec<String> {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/component-renders-dark.json"
    ))
    .expect("component renders golden fixture");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("valid golden json");
    value[key]
        .as_array()
        .expect("golden array")
        .iter()
        .map(|line| line.as_str().expect("golden line").to_string())
        .collect()
}

fn assert_matches_golden(key: &str, actual: Vec<String>) {
    let stripped: Vec<String> = actual.into_iter().map(|line| strip_ansi(&line)).collect();
    assert_eq!(stripped, component_golden(key), "render mismatch for {key}");
}

#[test]
fn extension_selector_and_input_render_like_the_ts_component() {
    theme();
    install_keybindings();

    let mut selector = ExtensionSelectorComponent::new(
        "Pick",
        vec!["a".to_string(), "b".to_string(), "c".to_string()],
        |_| {},
        || {},
        ExtensionSelectorOptions::default(),
    );
    assert_matches_golden("extensionSelector", selector.render(80));
    selector.handle_input("\x1b[B");
    selector.handle_input("\x1b[B");
    selector.handle_input("\x1b[B");
    assert_matches_golden("extensionSelectorDown", selector.render(80));

    let mut input = ExtensionInputComponent::new(
        "Input",
        Some("ph"),
        |_| {},
        || {},
        ExtensionInputOptions {
            initial_value: Some("hello".to_string()),
            description: Some("Describe it".to_string()),
            ..ExtensionInputOptions::default()
        },
    );
    assert_matches_golden("extensionInput", input.render(80));
}
