//! `test/mouse-components.test.ts` 的移植（9 个 it）。
//!
//! 与 TS 的差异：Editor 组件需要自己的 [`EditorTui`]（TS 的 `new Editor(tui, theme)` 复用同一个
//! TUI），见 [`mounted_editor`]。视口断言同样改读 [`TuiAltScreen::get_screen_lines`]
//! （见 `tui_alt_screen.rs` 的说明）。

#[path = "common/mod.rs"]
mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::TestTerminal;
use kiana_tui::alt_screen::{
    CopySelectionDispatch, CopySelectionDone, CopySelectionResult, TuiAltScreen,
    TuiAltScreenOptions,
};
use kiana_tui::components::editor::{Editor, EditorOptions, EditorTheme, EditorTui};
use kiana_tui::components::input::{Input, InputOptions};
use kiana_tui::components::select_list::{
    SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme,
};
use kiana_tui::components::settings_list::{
    SettingItem, SettingsList, SettingsListOptions, SettingsListTheme,
};
use kiana_tui::layout_node::SharedComponent;
use kiana_tui::terminal_colors::Terminal;
use kiana_tui::tui::{
    Component, FocusTarget, Focusable, OverlayAnchor, OverlayOptions, SizeValue, Tui,
    TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType, TuiStopOptions,
};

fn mouse(
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

/// `new TuiAltScreen(terminal, undefined, undefined)`。
fn alt(columns: usize, rows: usize) -> (TuiAltScreen, Rc<RefCell<String>>) {
    alt_with(columns, rows, TuiAltScreenOptions::default())
}

fn alt_with(
    columns: usize,
    rows: usize,
    options: TuiAltScreenOptions,
) -> (TuiAltScreen, Rc<RefCell<String>>) {
    let terminal = TestTerminal::with_size(columns, rows);
    let output = terminal.output_handle();
    (
        TuiAltScreen::new(Box::new(terminal), None, None, options),
        output,
    )
}

fn select_theme() -> SelectListTheme {
    SelectListTheme::plain()
}

fn settings_theme() -> SettingsListTheme {
    SettingsListTheme::plain()
}

/// 把共享组件句柄装进 `Box<dyn Component>`。
struct SharedRef(SharedComponent);

impl Component for SharedRef {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }
    fn handle_input(&mut self, data: &str) {
        self.0.borrow_mut().handle_input(data);
    }
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.0.borrow_mut().handle_mouse(event)
    }
    fn wants_key_release(&self) -> bool {
        self.0.borrow().wants_key_release()
    }
    fn handles_input(&self) -> bool {
        self.0.borrow().handles_input()
    }
    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }
    fn for_each_container_child(&self, f: &mut dyn FnMut(&dyn Component) -> bool) {
        self.0.borrow().for_each_container_child(f);
    }
    fn for_each_container_child_mut(
        &mut self,
        f: &mut dyn FnMut(&mut (dyn Component + 'static)) -> bool,
    ) {
        self.0.borrow_mut().for_each_container_child_mut(f);
    }
    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        if self.0.borrow_mut().as_focusable().is_some() {
            Some(self)
        } else {
            None
        }
    }
    fn layout_node(&self) -> Option<kiana_tui::layout_node::LayoutNode> {
        self.0.borrow().layout_node()
    }
}

impl Focusable for SharedRef {
    fn focused(&self) -> bool {
        let mut inner = self.0.borrow_mut();
        inner
            .as_focusable()
            .map(|focusable| focusable.focused())
            .unwrap_or(false)
    }
    fn set_focused(&mut self, focused: bool) {
        if let Some(focusable) = self.0.borrow_mut().as_focusable() {
            focusable.set_focused(focused);
        }
    }
}

/// `InputOverlay extends Container`：转发按键给嵌套的 Input。
struct InputOverlay {
    input: Rc<RefCell<Input>>,
    container: kiana_tui::tui::Container,
}

impl InputOverlay {
    fn new() -> Self {
        let input = Rc::new(RefCell::new(Input::new(InputOptions::default())));
        let mut container = kiana_tui::tui::Container::new();
        container.add_child(Box::new(SharedRef(input.clone())));
        InputOverlay { input, container }
    }
}

impl Component for InputOverlay {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.container.render(width)
    }
    fn handle_input(&mut self, data: &str) {
        self.input.borrow_mut().handle_input(data);
    }
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.container.handle_mouse(event)
    }
    fn invalidate(&mut self) {
        self.container.invalidate();
    }
    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        self.container.container_children()
    }
    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        self.container.container_children_mut()
    }
}

/// `positions a single-line input cursor on press`
#[test]
fn positions_a_single_line_input_cursor_on_press() {
    let mut input = Input::new(InputOptions::default());
    input.set_value("hello");
    input.render(20);

    assert_eq!(
        input
            .handle_mouse(&mouse(TuiMouseEventType::Press, 4, 0, 20, 1))
            .and_then(|result| result.handled),
        Some(true)
    );
    input.handle_input("X");
    assert_eq!(input.get_value(), "heXllo");
}

/// `selects and activates list rows`
#[test]
fn selects_and_activates_list_rows() {
    let selected: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&selected);
    let mut list = SelectList::new(
        vec![
            SelectItem::with_label("a", "A"),
            SelectItem::with_label("b", "B"),
            SelectItem::with_label("c", "C"),
            SelectItem::with_label("d", "D"),
            SelectItem::with_label("e", "E"),
        ],
        3,
        select_theme(),
        SelectListLayoutOptions::default(),
    );
    list.on_select = Some(Box::new(move |item| {
        *sink.borrow_mut() = Some(item.value.clone());
    }));

    assert_eq!(
        list.handle_mouse(&mouse(TuiMouseEventType::Press, 1, 2, 40, 3))
            .and_then(|result| result.handled),
        Some(true)
    );
    assert_eq!(
        list.get_selected_item().map(|item| item.value.as_str()),
        Some("c")
    );
    assert_eq!(
        list.handle_mouse(&mouse(TuiMouseEventType::Click, 1, 2, 40, 3))
            .and_then(|result| result.handled),
        Some(true)
    );
    assert_eq!(*selected.borrow(), Some("c".to_string()));
}

/// `activates settings rows`
#[test]
fn activates_settings_rows() {
    let changes: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&changes);
    let items: Vec<SettingItem> = [
        ("mode", "Mode", "one", vec!["one", "two"]),
        ("other", "Other", "off", vec!["off", "on"]),
        ("third", "Third", "low", vec!["low", "high"]),
        ("fourth", "Fourth", "x", vec!["x", "y"]),
    ]
    .into_iter()
    .map(|(id, label, current, values)| SettingItem {
        id: id.to_string(),
        label: label.to_string(),
        description: None,
        current_value: current.to_string(),
        values: Some(values.into_iter().map(str::to_string).collect()),
        submenu: None,
    })
    .collect();
    let mut list = SettingsList::new(
        items,
        3,
        settings_theme(),
        Box::new(move |id, value| {
            sink.borrow_mut().push((id.to_string(), value.to_string()));
        }),
        Box::new(|| {}),
        SettingsListOptions::default(),
    );

    list.handle_mouse(&mouse(TuiMouseEventType::Press, 1, 2, 40, 5));
    list.handle_mouse(&mouse(TuiMouseEventType::Click, 1, 2, 40, 5));
    assert_eq!(
        *changes.borrow(),
        vec![("third".to_string(), "high".to_string())]
    );
}

/// `ignores hover and clicks visible select-list row {row} after scrolling`
#[test]
fn ignores_hover_and_clicks_visible_select_list_rows_after_scrolling() {
    for row in [0usize, 4usize] {
        let changes: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let selected: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let changes_sink = Rc::clone(&changes);
        let selected_sink = Rc::clone(&selected);
        let mut list = SelectList::new(
            (0..12)
                .map(|index| {
                    SelectItem::with_label(format!("item-{index}"), format!("Item {index}"))
                })
                .collect(),
            5,
            select_theme(),
            SelectListLayoutOptions::default(),
        );
        list.on_selection_change = Some(Box::new(move |item| {
            changes_sink.borrow_mut().push(item.value.clone());
        }));
        list.on_select = Some(Box::new(move |item| {
            *selected_sink.borrow_mut() = Some(item.value.clone());
        }));
        list.set_selected_index(5);
        let mut wheel = mouse(TuiMouseEventType::Wheel, 1, row as isize, 80, 10);
        wheel.wheel_delta = Some(1.0);
        list.handle_mouse(&wheel);
        assert_eq!(
            list.get_selected_item().map(|item| item.value.as_str()),
            Some("item-6")
        );
        assert_eq!(*changes.borrow(), vec!["item-6".to_string()]);
        let before = list.render(80);
        assert!(
            before[row].ends_with(&format!("Item {}", 4 + row)),
            "row {row}: {:?}",
            before[row]
        );

        for y in [0usize, 1, 2, 3, 4, row] {
            let mut move_event = mouse(TuiMouseEventType::Move, 1, y as isize, 80, 10);
            move_event.button = TuiMouseButton::None;
            assert!(list.handle_mouse(&move_event).is_none());
            assert_eq!(list.render(80), before);
        }
        assert_eq!(
            list.get_selected_item().map(|item| item.value.as_str()),
            Some("item-6")
        );
        assert_eq!(*changes.borrow(), vec!["item-6".to_string()]);
        assert!(selected.borrow().is_none());

        list.handle_mouse(&mouse(TuiMouseEventType::Press, 1, row as isize, 80, 10));
        list.render(80);
        list.handle_mouse(&mouse(TuiMouseEventType::Click, 1, row as isize, 80, 10));
        assert_eq!(*selected.borrow(), Some(format!("item-{}", 4 + row)));
        assert_eq!(
            *changes.borrow(),
            vec!["item-6".to_string(), format!("item-{}", 4 + row)]
        );
    }
}

/// `ignores hover and clicks visible settings row {row} after scrolling`
#[test]
fn ignores_hover_and_clicks_visible_settings_rows_after_scrolling() {
    for row in [0usize, 4usize] {
        let changes: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&changes);
        let items: Vec<SettingItem> = (0..12)
            .map(|index| SettingItem {
                id: format!("item-{index}"),
                label: format!("Item {index}"),
                description: Some(format!("Description {index}")),
                current_value: "off".to_string(),
                values: Some(vec!["off".to_string(), "on".to_string()]),
                submenu: None,
            })
            .collect();
        let mut list = SettingsList::new(
            items,
            5,
            settings_theme(),
            Box::new(move |id, value| {
                sink.borrow_mut().push((id.to_string(), value.to_string()));
            }),
            Box::new(|| {}),
            SettingsListOptions {
                enable_search: Some(true),
            },
        );
        list.select_item("item-5");
        let mut wheel = mouse(TuiMouseEventType::Wheel, 1, (row + 2) as isize, 80, 10);
        wheel.wheel_delta = Some(1.0);
        list.handle_mouse(&wheel);
        let before = list.render(80);
        assert!(before[4].starts_with("> Item 6"), "{:?}", before[4]);
        assert!(
            before[row + 2].contains(&format!("Item {} ", 4 + row)),
            "{:?}",
            before[row + 2]
        );

        for y in [0usize, 1, 2, 3, 4, row] {
            let mut move_event = mouse(TuiMouseEventType::Move, 1, (y + 2) as isize, 80, 10);
            move_event.button = TuiMouseButton::None;
            assert!(list.handle_mouse(&move_event).is_none());
            assert_eq!(list.render(80), before);
        }
        assert!(changes.borrow().is_empty());

        list.handle_mouse(&mouse(
            TuiMouseEventType::Press,
            1,
            (row + 2) as isize,
            80,
            10,
        ));
        list.render(80);
        list.handle_mouse(&mouse(
            TuiMouseEventType::Click,
            1,
            (row + 2) as isize,
            80,
            10,
        ));
        assert_eq!(
            *changes.borrow(),
            vec![(format!("item-{}", 4 + row), "on".to_string())]
        );
    }
}

/// `keeps a delegating overlay focused when its nested input is clicked`
#[test]
fn keeps_a_delegating_overlay_focused_when_its_nested_input_is_clicked() {
    let terminal = TestTerminal::with_size(20, 4);
    let mut tui = TuiAltScreen::new(
        Box::new(terminal),
        None,
        None,
        TuiAltScreenOptions::default(),
    );
    let overlay = InputOverlay::new();
    let input = Rc::clone(&overlay.input);
    input.borrow_mut().set_value("hi");
    tui.start();
    tui.show_overlay(
        Box::new(overlay),
        OverlayOptions {
            anchor: Some(OverlayAnchor::TopLeft),
            width: Some(SizeValue::Absolute(20.0)),
            ..Default::default()
        },
    );
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;5;1M");
    tui.handle_terminal_input("\x1b[<0;5;1m");
    tui.handle_terminal_input("!");
    tui.render_now(false);

    assert_eq!(input.borrow().get_value(), "hi!");
    assert!(matches!(tui.focus(), Some(FocusTarget::Overlay { .. })));
    tui.stop(TuiStopOptions::default());
}

/// 子菜单宿主：把按键转给嵌套的 SelectList（TS `SubmenuHost`）。
struct SubmenuHost {
    list: Rc<RefCell<SelectList>>,
    container: kiana_tui::tui::Container,
}

impl SubmenuHost {
    fn new(done: kiana_tui::components::settings_list::SettingsDoneCallback) -> Self {
        let done = Rc::new(RefCell::new(done));
        let list = Rc::new(RefCell::new(SelectList::new(
            vec![
                SelectItem::with_label("first", "First"),
                SelectItem::with_label("second", "Second"),
            ],
            5,
            select_theme(),
            SelectListLayoutOptions::default(),
        )));
        let select_done = Rc::clone(&done);
        list.borrow_mut().on_select = Some(Box::new(move |item| {
            (select_done.borrow_mut())(Some(item.value.clone()), None);
        }));
        let cancel_done = Rc::clone(&done);
        list.borrow_mut().on_cancel = Some(Box::new(move || {
            (cancel_done.borrow_mut())(None, None);
        }));
        let mut container = kiana_tui::tui::Container::new();
        container.add_child(Box::new(SharedRef(list.clone())));
        SubmenuHost { list, container }
    }
}

impl Component for SubmenuHost {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.container.render(width)
    }
    fn handle_input(&mut self, data: &str) {
        self.list.borrow_mut().handle_input(data);
    }
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.container.handle_mouse(event)
    }
    fn invalidate(&mut self) {
        self.container.invalidate();
    }
    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        self.container.container_children()
    }
    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        self.container.container_children_mut()
    }
}

/// `keeps a settings list focused when a click in its submenu closes the submenu`
#[test]
fn keeps_a_settings_list_focused_when_a_click_in_its_submenu_closes_the_submenu() {
    let terminal = TestTerminal::with_size(30, 6);
    let mut tui = TuiAltScreen::new(
        Box::new(terminal),
        None,
        None,
        TuiAltScreenOptions::default(),
    );
    let changes: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&changes);
    let items = vec![
        SettingItem {
            id: "theme".to_string(),
            label: "Theme".to_string(),
            description: None,
            current_value: "first".to_string(),
            values: None,
            submenu: Some(Rc::new(|_value: &str, done| {
                Box::new(SubmenuHost::new(done))
            })),
        },
        SettingItem {
            id: "other".to_string(),
            label: "Other".to_string(),
            description: None,
            current_value: "off".to_string(),
            values: Some(vec!["off".to_string(), "on".to_string()]),
            submenu: None,
        },
    ];
    let list = Rc::new(RefCell::new(SettingsList::new(
        items,
        5,
        settings_theme(),
        Box::new(move |id, value| {
            sink.borrow_mut().push((id.to_string(), value.to_string()));
        }),
        Box::new(|| {}),
        SettingsListOptions::default(),
    )));
    tui.add_child_shared(list.clone() as SharedComponent);
    let target = tui
        .focus_target_of_component(&(list.clone() as SharedComponent))
        .expect("settings list mounted");
    tui.set_focus(Some(target));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\r");
    tui.render_now(false);
    // 在子菜单第二行上按下并释放会选中它并关闭子菜单。
    tui.handle_terminal_input("\x1b[<0;3;2M");
    tui.handle_terminal_input("\x1b[<0;3;2m");
    tui.render_now(false);
    assert_eq!(
        *changes.borrow(),
        vec![("theme".to_string(), "second".to_string())]
    );
    assert_eq!(tui.focus(), Some(target));

    // 按键回到可见列表，而不是已关闭的子菜单。
    tui.handle_terminal_input("\x1b[B");
    tui.handle_terminal_input("\r");
    tui.render_now(false);
    assert_eq!(
        *changes.borrow(),
        vec![
            ("theme".to_string(), "second".to_string()),
            ("other".to_string(), "on".to_string()),
        ]
    );
    tui.stop(TuiStopOptions::default());
}

/// 构造一个挂在 `TuiAltScreen` 里的 `Editor`（TS `new Editor(tui, editorTheme)`）。
///
/// TS 的 Editor 直接接收同一个 `tui`（只用它的 `terminal.rows` 与 `requestRender()`）；
/// Rust 侧 Editor 依赖 [`EditorTui`]，因此用同一尺寸的测试终端 + no-op 重绘回调，
/// 视口内容仍由 `TuiAltScreen` 渲染（测试断言的是编辑器的文本/光标/焦点）。
fn mounted_editor(tui: &mut TuiAltScreen, columns: usize, rows: usize) -> Rc<RefCell<Editor>> {
    let terminal: Rc<RefCell<Box<dyn Terminal>>> = Rc::new(RefCell::new(Box::new(
        TestTerminal::with_size(columns, rows),
    )));
    let editor = Rc::new(RefCell::new(Editor::new(
        EditorTui::from_terminal(terminal),
        EditorTheme::plain(),
        EditorOptions::default(),
    )));
    tui.add_child_shared(Rc::clone(&editor) as SharedComponent);
    editor
}

/// `positions and focuses the multiline editor through alternate-screen dispatch`
#[test]
fn positions_and_focuses_the_multiline_editor_through_alternate_screen_dispatch() {
    let (mut tui, _output) = alt(20, 6);
    let editor = mounted_editor(&mut tui, 20, 6);
    editor.borrow_mut().set_text("hello");
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;3;2M");
    tui.handle_terminal_input("\x1b[<0;3;2m");
    tui.handle_terminal_input("X");
    tui.render_now(false);

    assert_eq!(editor.borrow().get_text(), "heXllo");
    let target = tui
        .focus_target_of_component(&(Rc::clone(&editor) as SharedComponent))
        .expect("editor mounted");
    assert_eq!(tui.focus(), Some(target));
    tui.stop(TuiStopOptions::default());
}

/// `selects and copies editor text on drag instead of moving the cursor`
#[test]
fn selects_and_copies_editor_text_on_drag_instead_of_moving_the_cursor() {
    let copied: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&copied);
    let (mut tui, _output) = alt_with(
        20,
        6,
        TuiAltScreenOptions {
            copy_selection: Some(std::rc::Rc::new(
                move |text: &str, _done: CopySelectionDone| {
                    sink.borrow_mut().push(text.to_string());
                    CopySelectionDispatch::Ready(CopySelectionResult::Ok)
                },
            )),
            ..Default::default()
        },
    );
    let editor = mounted_editor(&mut tui, 20, 6);
    editor.borrow_mut().set_text("hello world");
    tui.start();
    tui.render_now(false);
    let cursor_before = editor.borrow().get_cursor();

    tui.handle_terminal_input("\x1b[<0;1;2M");
    tui.handle_terminal_input("\x1b[<32;5;2M");
    tui.handle_terminal_input("\x1b[<0;5;2m");
    tui.render_now(false);

    assert_eq!(*copied.borrow(), vec!["hello".to_string()]);
    assert_eq!(editor.borrow().get_cursor(), cursor_before);
    tui.stop(TuiStopOptions::default());
}
