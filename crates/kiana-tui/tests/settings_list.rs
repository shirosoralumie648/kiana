//! `test/settings-list.test.ts` 的 Rust 移植。

use std::cell::RefCell;
use std::rc::Rc;

use kiana_tui::components::settings_list::SettingsListOptions;
use kiana_tui::{Component, SettingItem, SettingsList, SettingsListTheme};

fn items() -> Vec<SettingItem> {
    let mut item = SettingItem::new("tui-mode", "TUI mode", "regular");
    item.values = Some(vec!["regular".to_string(), "fullscreen".to_string()]);
    vec![item]
}

#[test]
fn includes_spaces_in_an_active_search_instead_of_changing_the_selected_setting() {
    let changes: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let changes_sink = Rc::clone(&changes);
    let mut list = SettingsList::new(
        items(),
        10,
        SettingsListTheme::plain(),
        Box::new(move |id: &str, value: &str| {
            changes_sink
                .borrow_mut()
                .push((id.to_string(), value.to_string()));
        }),
        Box::new(|| {}),
        SettingsListOptions {
            enable_search: Some(true),
        },
    );

    for character in "TUI mode".chars() {
        list.handle_input(&character.to_string());
    }

    assert!(changes.borrow().is_empty());
    let rendered = list.render(80);
    assert!(rendered[0].contains("TUI mode"));

    list.handle_input("\r");
    assert_eq!(
        *changes.borrow(),
        vec![("tui-mode".to_string(), "fullscreen".to_string())]
    );
}

#[test]
fn keeps_space_as_a_change_shortcut_before_a_search_query_is_entered() {
    let changes: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let changes_sink = Rc::clone(&changes);
    let mut list = SettingsList::new(
        items(),
        10,
        SettingsListTheme::plain(),
        Box::new(move |id: &str, value: &str| {
            changes_sink
                .borrow_mut()
                .push((id.to_string(), value.to_string()));
        }),
        Box::new(|| {}),
        SettingsListOptions {
            enable_search: Some(true),
        },
    );

    list.handle_input(" ");

    assert_eq!(
        *changes.borrow(),
        vec![("tui-mode".to_string(), "fullscreen".to_string())]
    );
}

// ---------------------------------------------------------------------------
// test/mouse-components.test.ts
// ---------------------------------------------------------------------------

use kiana_tui::{TuiMouseButton, TuiMouseEvent, TuiMouseEventType};

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

fn setting(id: &str, label: &str, current: &str, values: &[&str]) -> SettingItem {
    let mut item = SettingItem::new(id, label, current);
    item.values = Some(values.iter().map(|value| value.to_string()).collect());
    item
}

#[test]
fn activates_settings_rows() {
    let changes: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let changes_sink = Rc::clone(&changes);
    let mut list = SettingsList::new(
        vec![
            setting("mode", "Mode", "one", &["one", "two"]),
            setting("other", "Other", "off", &["off", "on"]),
            setting("third", "Third", "low", &["low", "high"]),
            setting("fourth", "Fourth", "x", &["x", "y"]),
        ],
        3,
        SettingsListTheme::plain(),
        Box::new(move |id: &str, value: &str| {
            changes_sink
                .borrow_mut()
                .push((id.to_string(), value.to_string()));
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

#[test]
fn ignores_hover_and_clicks_visible_settings_rows_after_scrolling() {
    for row in [0isize, 4] {
        let changes: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
        let changes_sink = Rc::clone(&changes);
        let items: Vec<SettingItem> = (0..12)
            .map(|index| {
                let mut item = setting(
                    &format!("item-{index}"),
                    &format!("Item {index}"),
                    "off",
                    &["off", "on"],
                );
                item.description = Some(format!("Description {index}"));
                item
            })
            .collect();
        let mut list = SettingsList::new(
            items,
            5,
            SettingsListTheme::plain(),
            Box::new(move |id: &str, value: &str| {
                changes_sink
                    .borrow_mut()
                    .push((id.to_string(), value.to_string()));
            }),
            Box::new(|| {}),
            SettingsListOptions {
                enable_search: Some(true),
            },
        );
        list.select_item("item-5");
        let mut wheel = mouse(TuiMouseEventType::Wheel, 1, row + 2, 80, 10);
        wheel.wheel_delta = Some(1.0);
        list.handle_mouse(&wheel);
        let before = list.render(80);
        assert!(before[4].starts_with("> Item 6"), "row 4: {:?}", before[4]);
        assert!(
            before[(row + 2) as usize].contains(&format!("Item {} ", 4 + row)),
            "row {}: {:?}",
            row + 2,
            before[(row + 2) as usize]
        );

        for y in [0isize, 1, 2, 3, 4, row] {
            let mut hover = mouse(TuiMouseEventType::Move, 1, y + 2, 80, 10);
            hover.button = TuiMouseButton::None;
            assert!(list.handle_mouse(&hover).is_none());
            assert_eq!(list.render(80), before);
        }
        assert!(changes.borrow().is_empty());

        list.handle_mouse(&mouse(TuiMouseEventType::Press, 1, row + 2, 80, 10));
        list.render(80);
        list.handle_mouse(&mouse(TuiMouseEventType::Click, 1, row + 2, 80, 10));
        assert_eq!(
            *changes.borrow(),
            vec![(format!("item-{}", 4 + row), "on".to_string())]
        );
    }
}

// ---------------------------------------------------------------------------
// test/mouse-components.test.ts（子菜单关闭后焦点/输入回到列表）
// ---------------------------------------------------------------------------

use kiana_tui::components::settings_list::SettingsDoneCallback;
use kiana_tui::{
    SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme, TuiMouseEventResult,
};

fn select_theme() -> SelectListTheme {
    use std::sync::Arc;

    let identity = || -> kiana_tui::components::text_component::StyleFn {
        Arc::new(|text: &str| text.to_string())
    };
    SelectListTheme {
        selected_prefix: identity(),
        selected_text: identity(),
        description: identity(),
        scroll_info: identity(),
        no_match: identity(),
    }
}

/// 类似 coding agent 的主题子菜单：把按键/鼠标转发给内部 [`SelectList`]。
struct SubmenuHost {
    list: SelectList,
}

impl SubmenuHost {
    fn new(done: SettingsDoneCallback) -> Self {
        let items = vec![
            SelectItem {
                value: "first".to_string(),
                label: "First".to_string(),
                description: None,
            },
            SelectItem {
                value: "second".to_string(),
                label: "Second".to_string(),
                description: None,
            },
        ];
        let mut list =
            SelectList::new(items, 5, select_theme(), SelectListLayoutOptions::default());
        let shared: Rc<RefCell<SettingsDoneCallback>> = Rc::new(RefCell::new(done));
        let select_sink = Rc::clone(&shared);
        list.on_select = Some(Box::new(move |item: &SelectItem| {
            let mut callback = select_sink.borrow_mut();
            callback.as_mut()(Some(item.value.clone()), None);
        }));
        let cancel_sink = Rc::clone(&shared);
        list.on_cancel = Some(Box::new(move || {
            let mut callback = cancel_sink.borrow_mut();
            callback.as_mut()(None, None);
        }));
        SubmenuHost { list }
    }
}

impl Component for SubmenuHost {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.list.render(width)
    }

    fn handle_input(&mut self, data: &str) {
        self.list.handle_input(data);
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.list.handle_mouse(event)
    }
}

#[test]
fn keeps_the_settings_list_usable_when_a_click_in_its_submenu_closes_it() {
    let changes: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let changes_sink = Rc::clone(&changes);
    let mut theme_item = SettingItem::new("theme", "Theme", "first");
    theme_item.submenu = Some(Rc::new(
        |_current: &str, done: SettingsDoneCallback| -> Box<dyn Component> {
            Box::new(SubmenuHost::new(done))
        },
    ));
    let mut list = SettingsList::new(
        vec![theme_item, setting("other", "Other", "off", &["off", "on"])],
        5,
        SettingsListTheme::plain(),
        Box::new(move |id: &str, value: &str| {
            changes_sink
                .borrow_mut()
                .push((id.to_string(), value.to_string()));
        }),
        Box::new(|| {}),
        SettingsListOptions::default(),
    );

    // 打开主题子菜单。
    list.handle_input("\r");
    // 在子菜单第二行按下并释放：选中并关闭子菜单。
    list.handle_mouse(&mouse(TuiMouseEventType::Press, 3, 1, 30, 6));
    list.handle_mouse(&mouse(TuiMouseEventType::Click, 3, 1, 30, 6));
    assert_eq!(
        *changes.borrow(),
        vec![("theme".to_string(), "second".to_string())]
    );

    // 按键重新到达可见的主列表，而不是已关闭的子菜单。
    list.handle_input("\x1b[B");
    list.handle_input("\r");
    assert_eq!(
        *changes.borrow(),
        vec![
            ("theme".to_string(), "second".to_string()),
            ("other".to_string(), "on".to_string()),
        ]
    );
}

// ---------------------------------------------------------------------------
// 回归：`truncateToWidth(text, width)` 的默认省略号 `"..."`（reviewer finding 4）。
// 期望值来自 TS 参照实现（identity 主题、cursor `"> "`）：
//   row w20  -> "...  Enter/Space to \u001b[0m...\u001b[0m"
//   row w5   -> "> \u001b[0m...\u001b[0m" / "  \u001b[0m...\u001b[0m"
//   row w1   -> "\u001b[0m.\u001b[0m"（省略号被裁到 1 列）
//   filter   -> "  No matching set\u001b[0m...\u001b[0m"
// ---------------------------------------------------------------------------

fn single_item_list(search: bool, item: SettingItem) -> SettingsList {
    SettingsList::new(
        vec![item],
        5,
        SettingsListTheme::plain(),
        Box::new(|_: &str, _: &str| {}),
        Box::new(|| {}),
        SettingsListOptions {
            enable_search: Some(search),
        },
    )
}

fn described_item() -> SettingItem {
    let mut item = SettingItem::new("a", "Alpha", "x");
    item.description = Some("description".to_string());
    item
}

#[test]
fn truncates_rows_and_hints_with_the_default_ellipsis() {
    let mut list = single_item_list(false, described_item());

    let rendered = list.render(20);
    assert_eq!(rendered[0], "> Alpha  x");
    assert_eq!(
        rendered.last().expect("hint line"),
        "  Enter/Space to \u{1b}[0m...\u{1b}[0m"
    );

    let rendered = list.render(5);
    assert_eq!(rendered[0], "> \u{1b}[0m...\u{1b}[0m");
    assert_eq!(
        rendered.last().expect("hint line"),
        "  \u{1b}[0m...\u{1b}[0m"
    );

    let rendered = list.render(1);
    assert_eq!(rendered[0], "\u{1b}[0m.\u{1b}[0m");
    assert_eq!(rendered.last().expect("hint line"), "\u{1b}[0m.\u{1b}[0m");
}

#[test]
fn truncates_long_rows_with_the_default_ellipsis() {
    let mut item = SettingItem::new(
        "a",
        "A very long label that overflows",
        "value-that-overflows",
    );
    item.description = None;
    let mut list = single_item_list(false, item);

    let rendered = list.render(12);
    assert_eq!(rendered[0], "> A very \u{1b}[0m...\u{1b}[0m");
    assert_eq!(
        rendered.last().expect("hint line"),
        "  Enter/S\u{1b}[0m...\u{1b}[0m"
    );

    let rendered = list.render(8);
    assert_eq!(rendered[0], "> A v\u{1b}[0m...\u{1b}[0m");
}

#[test]
fn truncates_the_no_matching_settings_message_with_the_default_ellipsis() {
    let mut list = single_item_list(true, described_item());
    for character in "zzz".chars() {
        list.handle_input(&character.to_string());
    }

    let rendered = list.render(20);
    assert_eq!(rendered[2], "  No matching set\u{1b}[0m...\u{1b}[0m");
    assert_eq!(
        rendered.last().expect("hint line"),
        "  Type to search \u{1b}[0m...\u{1b}[0m"
    );

    let rendered = list.render(10);
    assert_eq!(rendered[2], "  No ma\u{1b}[0m...\u{1b}[0m");
}

// ---------------------------------------------------------------------------
// 评审回归：子菜单 `done()` 必须**同步**执行（TS settings-list.ts:264-308）
// ---------------------------------------------------------------------------

/// 子菜单：`handle_input` 里记录 before/after 并在中间调用 `done()`。
struct DoneOnInput {
    done: Option<SettingsDoneCallback>,
    log: Rc<RefCell<Vec<String>>>,
}

impl Component for DoneOnInput {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec![String::new()]
    }

    fn handle_input(&mut self, _data: &str) {
        self.log.borrow_mut().push("before-done".to_string());
        if let Some(done) = self.done.as_mut() {
            done(Some("picked".to_string()), None);
        }
        self.log.borrow_mut().push("after-done".to_string());
    }
}

/// `done()` 在子菜单 `handleInput` 的栈上同步执行：`onChange` 必须出现在
/// `before-done` 与 `after-done` 之间（与 TS 的实际顺序逐项相同）。
#[test]
fn submenu_done_runs_on_change_synchronously_inside_handle_input() {
    let log: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let change_log = Rc::clone(&log);
    let factory_log = Rc::clone(&log);

    let mut item = SettingItem::new("s", "Setting", "initial");
    item.submenu = Some(Rc::new(move |_value: &str, done| {
        Box::new(DoneOnInput {
            done: Some(done),
            log: Rc::clone(&factory_log),
        })
    }));

    let mut list = SettingsList::new(
        vec![item],
        5,
        SettingsListTheme::plain(),
        Box::new(move |id: &str, value: &str| {
            change_log
                .borrow_mut()
                .push(format!("onChange:{id}:{value}"));
        }),
        Box::new(|| {}),
        SettingsListOptions::default(),
    );

    // 回车打开子菜单，随后任意输入被转交给子菜单，子菜单在 handle_input 内调用 done()。
    list.handle_input("\r");
    list.handle_input("x");
    log.borrow_mut().push("after-input".to_string());

    assert_eq!(
        *log.borrow(),
        vec![
            "before-done".to_string(),
            "onChange:s:picked".to_string(),
            "after-done".to_string(),
            "after-input".to_string(),
        ]
    );

    // 子菜单关闭后回到主列表，且值已更新。
    let rendered = list.render(80);
    assert!(
        rendered.iter().any(|line| line.contains("picked")),
        "value should be updated synchronously: {rendered:?}"
    );
}
