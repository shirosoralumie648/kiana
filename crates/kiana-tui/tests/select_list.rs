//! `test/select-list.test.ts` 的 Rust 移植。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use kiana_tui::components::text_component::StyleFn;
use kiana_tui::{
    Component, SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme, visible_width,
};

fn identity() -> StyleFn {
    Arc::new(|text: &str| text.to_string())
}

fn test_theme() -> SelectListTheme {
    SelectListTheme {
        selected_prefix: identity(),
        selected_text: identity(),
        description: identity(),
        scroll_info: identity(),
        no_match: identity(),
    }
}

fn item(value: &str, label: &str, description: Option<&str>) -> SelectItem {
    SelectItem {
        value: value.to_string(),
        label: label.to_string(),
        description: description.map(str::to_string),
    }
}

fn visible_index_of(line: &str, text: &str) -> usize {
    let index = line
        .find(text)
        .unwrap_or_else(|| panic!("line {line:?} does not contain {text:?}"));
    visible_width(&line[..index])
}

#[test]
fn normalizes_multiline_descriptions_to_single_line() {
    let items = vec![item("test", "test", Some("Line one\nLine two\nLine three"))];

    let mut list = SelectList::new(items, 5, test_theme(), SelectListLayoutOptions::default());
    let rendered = list.render(100);

    assert!(!rendered.is_empty());
    assert!(!rendered[0].contains('\n'));
    assert!(rendered[0].contains("Line one Line two Line three"));
}

#[test]
fn keeps_descriptions_aligned_when_the_primary_text_is_truncated() {
    let items = vec![
        item("short", "short", Some("short description")),
        item(
            "very-long-command-name-that-needs-truncation",
            "very-long-command-name-that-needs-truncation",
            Some("long description"),
        ),
    ];

    let mut list = SelectList::new(items, 5, test_theme(), SelectListLayoutOptions::default());
    let rendered = list.render(80);

    assert_eq!(
        visible_index_of(&rendered[0], "short description"),
        visible_index_of(&rendered[1], "long description")
    );
}

#[test]
fn uses_the_configured_minimum_primary_column_width() {
    let items = vec![
        item("a", "a", Some("first")),
        item("bb", "bb", Some("second")),
    ];

    let mut list = SelectList::new(
        items,
        5,
        test_theme(),
        SelectListLayoutOptions {
            min_primary_column_width: Some(12.0),
            max_primary_column_width: Some(20.0),
            ..Default::default()
        },
    );
    let rendered = list.render(80);

    // TS 这里用 `indexOf`（无 ANSI、`→` 是 1 个 UTF-16 单元）；按可见列解释同一断言。
    assert_eq!(visible_index_of(&rendered[0], "first"), 14);
    assert_eq!(visible_index_of(&rendered[1], "second"), 14);
}

#[test]
fn uses_the_configured_maximum_primary_column_width() {
    let items = vec![
        item(
            "very-long-command-name-that-needs-truncation",
            "very-long-command-name-that-needs-truncation",
            Some("first"),
        ),
        item("short", "short", Some("second")),
    ];

    let mut list = SelectList::new(
        items,
        5,
        test_theme(),
        SelectListLayoutOptions {
            min_primary_column_width: Some(12.0),
            max_primary_column_width: Some(20.0),
            ..Default::default()
        },
    );
    let rendered = list.render(80);

    assert_eq!(visible_index_of(&rendered[0], "first"), 22);
    assert_eq!(visible_index_of(&rendered[1], "second"), 22);
}

#[test]
fn allows_overriding_primary_truncation_while_preserving_description_alignment() {
    let items = vec![
        item(
            "very-long-command-name-that-needs-truncation",
            "very-long-command-name-that-needs-truncation",
            Some("first"),
        ),
        item("short", "short", Some("second")),
    ];

    let truncate_primary: Arc<
        dyn Fn(&kiana_tui::SelectListTruncatePrimaryContext) -> String + Send + Sync,
    > = Arc::new(|context| {
        if (context.text.len() as f64) <= context.max_width {
            return context.text.clone();
        }
        let keep = (context.max_width - 1.0).max(0.0) as usize;
        format!("{}…", context.text.chars().take(keep).collect::<String>())
    });

    let mut list = SelectList::new(
        items,
        5,
        test_theme(),
        SelectListLayoutOptions {
            min_primary_column_width: Some(12.0),
            max_primary_column_width: Some(12.0),
            truncate_primary: Some(truncate_primary),
        },
    );
    let rendered = list.render(80);

    assert!(rendered[0].contains('…'));
    assert_eq!(
        visible_index_of(&rendered[0], "first"),
        visible_index_of(&rendered[1], "second")
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

#[test]
fn selects_and_activates_list_rows() {
    let items: Vec<SelectItem> = ["a", "b", "c", "d", "e"]
        .into_iter()
        .map(|value| item(value, &value.to_uppercase(), None))
        .collect();
    let mut list = SelectList::new(items, 3, test_theme(), SelectListLayoutOptions::default());
    let selected: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&selected);
    list.on_select = Some(Box::new(move |item: &SelectItem| {
        *sink.borrow_mut() = Some(item.value.clone());
    }));

    let press = list
        .handle_mouse(&mouse(TuiMouseEventType::Press, 1, 2, 40, 3))
        .expect("press is handled");
    assert_eq!(press.handled, Some(true));
    assert_eq!(
        list.get_selected_item().map(|item| item.value.clone()),
        Some("c".to_string())
    );
    let click = list
        .handle_mouse(&mouse(TuiMouseEventType::Click, 1, 2, 40, 3))
        .expect("click is handled");
    assert_eq!(click.handled, Some(true));
    assert_eq!(*selected.borrow(), Some("c".to_string()));
}

#[test]
fn ignores_hover_and_clicks_visible_rows_after_scrolling() {
    for row in [0isize, 4] {
        let items: Vec<SelectItem> = (0..12)
            .map(|index| item(&format!("item-{index}"), &format!("Item {index}"), None))
            .collect();
        let mut list = SelectList::new(items, 5, test_theme(), SelectListLayoutOptions::default());
        let changes: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let selected: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let change_sink = Rc::clone(&changes);
        list.on_selection_change = Some(Box::new(move |item: &SelectItem| {
            change_sink.borrow_mut().push(item.value.clone());
        }));
        let select_sink = Rc::clone(&selected);
        list.on_select = Some(Box::new(move |item: &SelectItem| {
            *select_sink.borrow_mut() = Some(item.value.clone());
        }));
        list.set_selected_index(5);
        let mut wheel = mouse(TuiMouseEventType::Wheel, 1, row, 80, 10);
        wheel.wheel_delta = Some(1.0);
        list.handle_mouse(&wheel);
        assert_eq!(
            list.get_selected_item().map(|item| item.value.clone()),
            Some("item-6".to_string())
        );
        assert_eq!(*changes.borrow(), vec!["item-6".to_string()]);
        let before = list.render(80);
        assert!(
            before[row as usize].ends_with(&format!("Item {}", 4 + row)),
            "row {row}: {:?}",
            before[row as usize]
        );

        for y in [0isize, 1, 2, 3, 4, row] {
            let mut hover = mouse(TuiMouseEventType::Move, 1, y, 80, 10);
            hover.button = TuiMouseButton::None;
            assert!(list.handle_mouse(&hover).is_none());
            assert_eq!(list.render(80), before);
        }
        assert_eq!(
            list.get_selected_item().map(|item| item.value.clone()),
            Some("item-6".to_string())
        );
        assert_eq!(*changes.borrow(), vec!["item-6".to_string()]);
        assert!(selected.borrow().is_none());

        list.handle_mouse(&mouse(TuiMouseEventType::Press, 1, row, 80, 10));
        list.render(80);
        list.handle_mouse(&mouse(TuiMouseEventType::Click, 1, row, 80, 10));
        assert_eq!(*selected.borrow(), Some(format!("item-{}", 4 + row)));
        assert_eq!(
            *changes.borrow(),
            vec!["item-6".to_string(), format!("item-{}", 4 + row)]
        );
    }
}

// ---------------------------------------------------------------------------
// 回归：纯空白/换行的 description 归一化为 `""`，按 JS 真值语义视为没有描述
// （reviewer finding 5）。期望值来自 TS 参照实现。
// ---------------------------------------------------------------------------

#[test]
fn treats_whitespace_only_descriptions_as_absent() {
    for description in ["\n", " ", "\r\n", "\t"] {
        let items = vec![
            item(
                "very-long-command-name-that-needs-truncation",
                "very-long-command-name-that-needs-truncation",
                Some(description),
            ),
            item("x", "x", None),
        ];
        let mut list = SelectList::new(items, 5, test_theme(), SelectListLayoutOptions::default());
        let rendered = list.render(100);
        assert_eq!(
            rendered,
            vec![
                "→ very-long-command-name-that-needs-truncation".to_string(),
                "  x".to_string()
            ],
            "description {description:?} must not select the two-column layout"
        );
    }
}

#[test]
fn keeps_descriptions_that_become_non_empty_after_normalization() {
    let items = vec![item("a", "a", Some("  x  "))];
    let mut list = SelectList::new(items, 5, test_theme(), SelectListLayoutOptions::default());
    let rendered = list.render(100);
    assert_eq!(
        visible_index_of(&rendered[0], "x"),
        34,
        "two-column layout keeps the 32+2 primary column: {:?}",
        rendered[0]
    );
}

// ---------------------------------------------------------------------------
// 评审回归：`normalizeToSingleLine` 用 JS 的 `String.prototype.trim`
// ---------------------------------------------------------------------------
//
// JS `trim` 的空白集合含 U+FEFF、不含 U+0085；Rust `str::trim` 恰好相反。
// 描述只有这两个字符之一时，"有没有描述列" 会翻转。

/// `\u{FEFF}` 被 JS trim 掉 → 归一化为 `""` → 不保留描述列。
#[test]
fn description_of_only_bom_is_dropped() {
    let mut list = SelectList::new(
        vec![item("a", "a", Some("\u{FEFF}"))],
        5,
        test_theme(),
        SelectListLayoutOptions::default(),
    );
    assert_eq!(list.render(60), vec!["\u{2192} a"]);
}

/// `\u{0085}` 不在 JS 的空白集合里 → 保留 → 两列布局（描述列 31 个空格后跟它）。
#[test]
fn description_of_only_next_line_is_kept() {
    let mut list = SelectList::new(
        vec![item("a", "a", Some("\u{0085}"))],
        5,
        test_theme(),
        SelectListLayoutOptions::default(),
    );
    assert_eq!(
        list.render(60),
        vec![format!("\u{2192} a{}{}", " ".repeat(31), "\u{0085}")]
    );
}
