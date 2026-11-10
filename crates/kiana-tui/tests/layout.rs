//! `test/layout.test.ts` 的 Rust 移植。
//!
//! 差异：TS 的 1e9 行稀疏数组在 Rust 侧按 200_000 行实现（同样的裁剪断言；见 deviations）。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use kiana_tui::components::scroll_view::ScrollViewFollow;
use kiana_tui::components::text_component::StyleFn;
use kiana_tui::layout::{LayoutFrame, render_layout_frame};
use kiana_tui::layout_node::{SharedComponent, StackAlign};
use kiana_tui::terminal_image::{
    KittyEncodeOptions, KittyImageMetadata, encode_kitty, register_kitty_image_metadata,
};
use kiana_tui::{
    Component, HStack, ScrollView, ScrollViewOptions, ScrollViewScrollbar, StackChild,
    StackEntryOptions, StackOptions, Text, VStack, strip_terminal_sequences,
};

fn shared<T: Component + 'static>(component: T) -> SharedComponent {
    Rc::new(RefCell::new(component))
}

fn as_shared<T: Component + 'static>(rc: &Rc<RefCell<T>>) -> SharedComponent {
    let clone: Rc<RefCell<T>> = Rc::clone(rc);
    clone
}

fn frame(root: SharedComponent, width: f64, height: f64) -> LayoutFrame {
    render_layout_frame(root, width, height, &|| {})
}

fn visible_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|line| strip_terminal_sequences(line).trim_end().to_string())
        .collect()
}

fn text_entry(text: &str, options: StackEntryOptions) -> StackChild {
    StackChild::entry(Box::new(Text::with_padding(text, 0, 0)), options)
}

/// 用闭包实现 `render` 的测试组件。
struct FnComponent<F: FnMut(usize) -> Vec<String>> {
    f: F,
}

impl<F: FnMut(usize) -> Vec<String>> Component for FnComponent<F> {
    fn render(&mut self, width: usize) -> Vec<String> {
        (self.f)(width)
    }

    fn handles_input(&self) -> bool {
        false
    }
}

#[test]
fn allocates_vertical_grow_space_deterministically() {
    let root = shared(VStack::new(
        vec![
            text_entry(
                "top",
                StackEntryOptions {
                    basis: Some(1.0),
                    shrink: Some(0.0),
                    ..Default::default()
                },
            ),
            text_entry(
                "body",
                StackEntryOptions {
                    basis: Some(0.0),
                    grow: Some(1.0),
                    ..Default::default()
                },
            ),
        ],
        StackOptions::default(),
    ));

    let frame = frame(root, 10.0, 4.0);

    assert_eq!(
        frame
            .root
            .children
            .iter()
            .map(|child| child.rect.height)
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_eq!(visible_lines(&frame.lines), vec!["top", "body", "", ""]);
}

#[test]
fn does_not_render_fixed_basis_scroll_content_during_stack_measurement() {
    let render_count = Rc::new(Cell::new(0usize));
    let counter = Rc::clone(&render_count);
    let transcript = ScrollView::new(
        Box::new(FnComponent {
            f: move |_width: usize| {
                counter.set(counter.get() + 1);
                vec!["one".to_string(), "two".to_string(), "three".to_string()]
            },
        }),
        ScrollViewOptions::default(),
    )
    .expect("valid scroll view");

    let root = shared(VStack::new(
        vec![
            StackChild::entry(
                Box::new(transcript),
                StackEntryOptions {
                    basis: Some(0.0),
                    grow: Some(1.0),
                    ..Default::default()
                },
            ),
            text_entry("dock", StackEntryOptions::default()),
        ],
        StackOptions::default(),
    ));

    frame(root, 10.0, 3.0);
    assert_eq!(render_count.get(), 1);
}

#[test]
fn paints_only_clipped_rows_from_very_large_scroll_content() {
    // TS：1_000_000_000 行稀疏数组；Rust 侧缩小到 200_000（同样的裁剪断言）。
    let line_count = 200_000usize;
    let mut lines: Vec<String> = vec![String::new(); line_count];
    lines[line_count - 4] = "before".to_string();
    lines[line_count - 3] = "visible 1".to_string();
    lines[line_count - 2] = "visible 2".to_string();
    lines[line_count - 1] = "visible 3".to_string();
    let lines = Rc::new(lines);

    let transcript = ScrollView::new(
        Box::new(FnComponent {
            f: {
                let lines = Rc::clone(&lines);
                move |_width: usize| (*lines).clone()
            },
        }),
        ScrollViewOptions {
            follow: Some(ScrollViewFollow::End),
            ..Default::default()
        },
    )
    .expect("valid scroll view");

    let frame = frame(shared(transcript), 10.0, 3.0);
    assert_eq!(
        visible_lines(&frame.lines),
        vec!["visible 1", "visible 2", "visible 3"]
    );
}

#[test]
fn shrinks_entries_to_their_minimum_sizes() {
    let root = shared(VStack::new(
        vec![
            text_entry(
                "a1\na2\na3",
                StackEntryOptions {
                    shrink: Some(1.0),
                    min_size: Some(1.0),
                    ..Default::default()
                },
            ),
            text_entry(
                "b1\nb2\nb3",
                StackEntryOptions {
                    shrink: Some(0.0),
                    ..Default::default()
                },
            ),
        ],
        StackOptions::default(),
    ));

    let frame = frame(root, 10.0, 4.0);

    assert_eq!(
        frame
            .root
            .children
            .iter()
            .map(|child| child.rect.height)
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_eq!(visible_lines(&frame.lines), vec!["a1", "b1", "b2", "b3"]);
}

#[test]
fn includes_nested_minimum_sizes_in_intrinsic_stack_measurement() {
    let dock = VStack::new(
        vec![
            StackChild::Component(Box::new(Text::with_padding("top1\ntop2\ntop3", 0, 0))),
            text_entry(
                "selector",
                StackEntryOptions {
                    min_size: Some(3.0),
                    ..Default::default()
                },
            ),
            StackChild::Component(Box::new(Text::with_padding("below", 0, 0))),
            text_entry(
                "footer",
                StackEntryOptions {
                    min_size: Some(1.0),
                    ..Default::default()
                },
            ),
        ],
        StackOptions::default(),
    );
    let root = shared(VStack::new(
        vec![
            text_entry(
                "body",
                StackEntryOptions {
                    basis: Some(0.0),
                    grow: Some(1.0),
                    min_size: Some(1.0),
                    ..Default::default()
                },
            ),
            StackChild::entry(
                Box::new(dock),
                StackEntryOptions {
                    min_size: Some(1.0),
                    ..Default::default()
                },
            ),
        ],
        StackOptions::default(),
    ));

    let frame = frame(root, 10.0, 9.0);

    assert_eq!(
        visible_lines(&frame.lines),
        vec![
            "body", "top1", "top2", "top3", "selector", "", "", "below", "footer"
        ]
    );
}

#[test]
fn omits_gaps_around_invisible_entries() {
    let mut stack = VStack::new(
        vec![
            StackChild::Component(Box::new(Text::with_padding("one", 0, 0))),
            text_entry(
                "hidden",
                StackEntryOptions {
                    visible: Some(Rc::new(|_viewport| false)),
                    ..Default::default()
                },
            ),
            StackChild::Component(Box::new(Text::with_padding("two", 0, 0))),
        ],
        StackOptions {
            gap: Some(1.0),
            ..Default::default()
        },
    );

    assert_eq!(
        stack
            .render(10)
            .iter()
            .map(|line| strip_terminal_sequences(line).trim_end().to_string())
            .collect::<Vec<_>>(),
        vec!["one", "", "two"]
    );
}

#[test]
fn crops_kitty_images_at_a_scroll_views_lower_boundary() {
    let image_id = 124u64;
    let image_line = encode_kitty(
        "AAAA",
        KittyEncodeOptions {
            columns: Some(2.0),
            rows: Some(3.0),
            image_id: Some(image_id as f64),
            move_cursor: Some(false),
        },
    );
    register_kitty_image_metadata(KittyImageMetadata {
        image_id,
        columns: 2,
        rows: 3,
        width_px: 100.0,
        height_px: 100.0,
    });
    let transcript = ScrollView::new(
        Box::new(FnComponent {
            f: move |_width: usize| {
                vec![
                    "one".to_string(),
                    "two".to_string(),
                    image_line.clone(),
                    String::new(),
                    String::new(),
                ]
            },
        }),
        ScrollViewOptions::default(),
    )
    .expect("valid scroll view");
    let root = shared(VStack::new(
        vec![
            StackChild::entry(
                Box::new(transcript),
                StackEntryOptions {
                    basis: Some(0.0),
                    grow: Some(1.0),
                    ..Default::default()
                },
            ),
            text_entry("dock", StackEntryOptions::default()),
        ],
        StackOptions::default(),
    ));

    let frame = frame(root, 20.0, 4.0);
    assert!(
        frame.lines[2].contains("y=0,h=34,r=1"),
        "line: {:?}",
        frame.lines[2]
    );
}

#[test]
fn composes_horizontal_children_at_allocated_widths() {
    let root = shared(HStack::new(
        vec![
            text_entry(
                "left",
                StackEntryOptions {
                    basis: Some(6.0),
                    shrink: Some(0.0),
                    ..Default::default()
                },
            ),
            text_entry(
                "right",
                StackEntryOptions {
                    basis: Some(6.0),
                    shrink: Some(0.0),
                    ..Default::default()
                },
            ),
        ],
        StackOptions::default(),
    ));

    let frame = frame(root, 12.0, 1.0);
    assert_eq!(visible_lines(&frame.lines), vec!["left  right"]);
}

#[test]
fn does_not_paint_zero_width_horizontal_children() {
    let root = shared(HStack::new(
        vec![
            text_entry(
                "hidden",
                StackEntryOptions {
                    basis: Some(0.0),
                    shrink: Some(0.0),
                    ..Default::default()
                },
            ),
            text_entry(
                "shown",
                StackEntryOptions {
                    basis: Some(0.0),
                    grow: Some(1.0),
                    ..Default::default()
                },
            ),
        ],
        StackOptions::default(),
    ));

    let frame = frame(root, 5.0, 1.0);
    assert_eq!(visible_lines(&frame.lines), vec!["shown"]);
}

#[test]
fn tracks_follow_end_state_and_returns_unused_scroll_delta() {
    let scroll_view = Rc::new(RefCell::new(
        ScrollView::new(
            Box::new(Text::with_padding("1\n2\n3\n4\n5\n6", 0, 0)),
            ScrollViewOptions {
                follow: Some(ScrollViewFollow::End),
                primary: Some(true),
                ..Default::default()
            },
        )
        .expect("valid scroll view"),
    ));
    frame(as_shared(&scroll_view), 10.0, 3.0);
    assert_eq!(scroll_view.borrow().scroll_top(), 3);
    assert!(scroll_view.borrow().is_following_end());

    assert_eq!(scroll_view.borrow().scroll_by(-2.0), 0.0);
    assert_eq!(scroll_view.borrow().scroll_top(), 1);
    assert!(!scroll_view.borrow().is_following_end());
    assert_eq!(scroll_view.borrow().scroll_by(-3.0), -2.0);
    assert_eq!(scroll_view.borrow().scroll_top(), 0);
    assert_eq!(scroll_view.borrow().scroll_by(10.0), 7.0);
    assert_eq!(scroll_view.borrow().scroll_top(), 3);
    assert!(scroll_view.borrow().is_following_end());
}

#[test]
fn renders_a_proportional_glyph_scrollbar_with_an_expanded_active_thumb() {
    let source_lines = [
        "abcd界", "abcde2", "abcde3", "abcde4", "abcde5", "abcde6", "abcde7", "abcde8",
    ];
    let content_background = "\x1b[42m";
    let track_color = "\x1b[38;5;2m";
    let thumb_color = "\x1b[38;5;1m";
    let scrollbar_track_style: StyleFn =
        Arc::new(move |text: &str| format!("{track_color}{text}\x1b[39m"));
    let scrollbar_thumb_style: StyleFn =
        Arc::new(move |text: &str| format!("{thumb_color}{text}\x1b[39m"));
    let content_background_style: StyleFn =
        Arc::new(move |text: &str| format!("{content_background}{text}\x1b[49m"));
    let content = Text::with_background(
        source_lines.join("\n"),
        0,
        0,
        Some(content_background_style),
    );
    let scroll_view = Rc::new(RefCell::new(
        ScrollView::new(
            Box::new(content),
            ScrollViewOptions {
                scrollbar: Some(ScrollViewScrollbar::Auto),
                scrollbar_track_style: Some(scrollbar_track_style),
                scrollbar_thumb_style: Some(scrollbar_thumb_style),
                scrollbar_hide_delay_ms: Some(10.0),
                ..Default::default()
            },
        )
        .expect("valid scroll view"),
    ));

    let render = || frame(as_shared(&scroll_view), 6.0, 4.0).lines;
    let visible = |lines: &[String]| -> Vec<String> {
        lines
            .iter()
            .map(|line| strip_terminal_sequences(line))
            .collect()
    };

    let mut lines = render();
    assert_eq!(
        visible(&lines),
        source_lines[..4]
            .iter()
            .map(|line| (*line).to_string())
            .collect::<Vec<_>>()
    );

    scroll_view.borrow().scroll_by(2.0);
    lines = render();
    assert_eq!(
        visible(&lines),
        vec!["abcde│", "abcde┃", "abcde┃", "abcde│"]
    );
    assert_eq!(
        lines
            .iter()
            .map(|line| line.contains(track_color))
            .collect::<Vec<_>>(),
        vec![true, false, false, true]
    );
    assert_eq!(
        lines
            .iter()
            .map(|line| line.contains(thumb_color))
            .collect::<Vec<_>>(),
        vec![false, true, true, false]
    );

    scroll_view.borrow().set_scrollbar_active(true);
    lines = render();
    assert_eq!(
        visible(&lines),
        vec!["abcde│", "abcde█", "abcde█", "abcde│"]
    );
    assert_eq!(
        lines
            .iter()
            .map(|line| line.contains(thumb_color))
            .collect::<Vec<_>>(),
        vec![false, true, true, false]
    );
    assert!(lines[1].rfind(content_background).unwrap() < lines[1].rfind(thumb_color).unwrap());

    scroll_view.borrow().set_scrollbar_active(false);
    std::thread::sleep(Duration::from_millis(30));
    lines = render();
    assert_eq!(
        visible(&lines),
        source_lines[2..6]
            .iter()
            .map(|line| (*line).to_string())
            .collect::<Vec<_>>()
    );

    scroll_view.borrow().scroll_to_end();
    lines = render();
    assert_eq!(
        visible(&lines),
        vec!["abcde│", "abcde│", "abcde┃", "abcde┃"]
    );

    scroll_view.borrow().scroll_to_start();
    lines = render();
    assert_eq!(visible(&lines)[0], "abcd ┃");

    // follow-end 的内容增长。
    let followed_content = Rc::new(RefCell::new(Text::with_padding(
        source_lines.join("\n"),
        0,
        0,
    )));
    let followed = Rc::new(RefCell::new(
        ScrollView::from_shared(
            as_shared(&followed_content),
            ScrollViewOptions {
                follow: Some(ScrollViewFollow::End),
                scrollbar: Some(ScrollViewScrollbar::Auto),
                scrollbar_track_style: Some(Arc::new(move |text: &str| {
                    format!("{track_color}{text}\x1b[39m")
                })),
                scrollbar_thumb_style: Some(Arc::new(move |text: &str| {
                    format!("{thumb_color}{text}\x1b[39m")
                })),
                ..Default::default()
            },
        )
        .expect("valid scroll view"),
    ));
    frame(as_shared(&followed), 6.0, 4.0);
    assert_eq!(followed.borrow().scroll_top(), 4);
    followed_content
        .borrow_mut()
        .set_text(format!("{}\nabcde9", source_lines.join("\n")));
    let growth_frame = frame(as_shared(&followed), 6.0, 4.0);
    assert_eq!(followed.borrow().scroll_top(), 5);
    assert!(growth_frame.lines.iter().all(|line| {
        let stripped = strip_terminal_sequences(line);
        !stripped.contains('│') && !stripped.contains('┃')
    }));

    let fitting_content = Text::with_padding("1\n2", 0, 0);
    let automatic = Rc::new(RefCell::new(
        ScrollView::new(
            Box::new(fitting_content),
            ScrollViewOptions {
                scrollbar: Some(ScrollViewScrollbar::Auto),
                scrollbar_thumb_style: Some(Arc::new(move |text: &str| {
                    format!("{thumb_color}{text}\x1b[39m")
                })),
                ..Default::default()
            },
        )
        .expect("valid scroll view"),
    ));
    frame(as_shared(&automatic), 6.0, 4.0);
    automatic.borrow().scroll_by(1.0);
    assert!(
        frame(as_shared(&automatic), 6.0, 4.0)
            .lines
            .iter()
            .all(|line| {
                let stripped = strip_terminal_sequences(line);
                !stripped.contains('│') && !stripped.contains('┃')
            })
    );

    let always_fitting = ScrollView::new(
        Box::new(Text::with_padding("1\n2", 0, 0)),
        ScrollViewOptions {
            scrollbar: Some(ScrollViewScrollbar::Always),
            scrollbar_thumb_style: Some(Arc::new(move |text: &str| {
                format!("{thumb_color}{text}\x1b[39m")
            })),
            ..Default::default()
        },
    )
    .expect("valid scroll view");
    let always_fitting_frame = frame(shared(always_fitting), 6.0, 4.0);
    assert_eq!(always_fitting_frame.root.children[0].rect.width, 5);
    assert!(
        visible(&always_fitting_frame.lines)
            .iter()
            .all(|line| line.ends_with('┃'))
    );

    let always_content = Text::with_background(
        source_lines.join("\n"),
        0,
        0,
        Some(Arc::new(move |text: &str| {
            format!("{content_background}{text}\x1b[49m")
        })),
    );
    let always_overflowing = ScrollView::new(
        Box::new(always_content),
        ScrollViewOptions {
            scrollbar: Some(ScrollViewScrollbar::Always),
            scrollbar_track_style: Some(Arc::new(move |text: &str| {
                format!("{track_color}{text}\x1b[39m")
            })),
            scrollbar_thumb_style: Some(Arc::new(move |text: &str| {
                format!("{thumb_color}{text}\x1b[39m")
            })),
            ..Default::default()
        },
    )
    .expect("valid scroll view");
    let always_overflowing_frame = frame(shared(always_overflowing), 6.0, 4.0);
    assert_eq!(always_overflowing_frame.root.children[0].rect.width, 5);
    assert_eq!(
        visible(&always_overflowing_frame.lines)
            .iter()
            .filter(|line| line.ends_with('┃'))
            .count(),
        2
    );
    assert_eq!(
        visible(&always_overflowing_frame.lines)
            .iter()
            .filter(|line| line.ends_with('│'))
            .count(),
        2
    );
    for line in &always_overflowing_frame.lines {
        let scrollbar_style_index = line
            .rfind(track_color)
            .max(line.rfind(thumb_color))
            .expect("scrollbar style present");
        // TS `lastIndexOf(pattern, scrollbarStyleIndex)`：在样式列之前向后找。
        let reserved_column_reset_index = line[..scrollbar_style_index]
            .rfind("\x1b[0m\x1b]8;;\x07")
            .expect("reserved column reset");
        assert!(
            reserved_column_reset_index
                > line
                    .rfind(content_background)
                    .expect("content background present")
        );
    }

    let thumb_height_for = |content_height: usize| -> usize {
        let text = Text::with_padding(vec!["x"; content_height].join("\n"), 0, 0);
        let sized = Rc::new(RefCell::new(
            ScrollView::new(
                Box::new(text),
                ScrollViewOptions {
                    scrollbar: Some(ScrollViewScrollbar::Auto),
                    scrollbar_thumb_style: Some(Arc::new(move |text: &str| {
                        format!("{thumb_color}{text}\x1b[39m")
                    })),
                    ..Default::default()
                },
            )
            .expect("valid scroll view"),
        ));
        frame(as_shared(&sized), 6.0, 20.0);
        sized.borrow().scroll_by(1.0);
        frame(as_shared(&sized), 6.0, 20.0)
            .lines
            .iter()
            .filter(|line| strip_terminal_sequences(line).ends_with('┃'))
            .count()
    };
    assert_eq!(thumb_height_for(21), 19);
    assert_eq!(thumb_height_for(40), 10);
    assert_eq!(thumb_height_for(100), 4);
    assert_eq!(thumb_height_for(400), 2);
}

#[test]
fn preserves_only_the_underlying_background_beneath_overlay_scrollbar_glyphs() {
    let background = "\x1b[42m";
    let border_foreground = "\x1b[31m";
    let content = FnComponent {
        f: move |width: usize| {
            vec![
                format!(
                    "{background}{}{border_foreground}│\x1b[39m\x1b[49m",
                    "x".repeat(width.saturating_sub(1))
                );
                8
            ]
        },
    };
    let scroll_view = Rc::new(RefCell::new(
        ScrollView::new(
            Box::new(content),
            ScrollViewOptions {
                scrollbar: Some(ScrollViewScrollbar::Auto),
                scrollbar_track_style: Some(Arc::new(|text: &str| text.to_string())),
                scrollbar_thumb_style: Some(Arc::new(|text: &str| text.to_string())),
                ..Default::default()
            },
        )
        .expect("valid scroll view"),
    ));
    frame(as_shared(&scroll_view), 6.0, 4.0);
    scroll_view.borrow().scroll_by(1.0);
    let frame = frame(as_shared(&scroll_view), 6.0, 4.0);

    assert_eq!(
        frame
            .lines
            .iter()
            .map(|line| strip_terminal_sequences(line))
            .collect::<Vec<_>>(),
        vec!["xxxxx│", "xxxxx┃", "xxxxx┃", "xxxxx│"]
    );
    for line in &frame.lines {
        assert!(line.contains(background));
        assert!(!line.contains(border_foreground));
        assert!(line.contains(&format!("\x1b[0m\x1b]8;;\x07{background}")));
    }
}

#[test]
fn updates_reserved_scrollbar_layout_at_runtime() {
    let scroll_view = Rc::new(RefCell::new(
        ScrollView::new(
            Box::new(Text::with_padding("123456", 0, 0)),
            ScrollViewOptions {
                scrollbar: Some(ScrollViewScrollbar::Always),
                ..Default::default()
            },
        )
        .expect("valid scroll view"),
    ));
    let root: SharedComponent = shared(HStack::new(
        vec![StackChild::Shared(as_shared(&scroll_view))],
        StackOptions {
            align: Some(StackAlign::Start),
            ..Default::default()
        },
    ));
    let render = || frame(Rc::clone(&root), 6.0, 2.0);

    let always = render();
    assert_eq!(visible_lines(&always.lines), vec!["12345┃", "6    ┃"]);
    assert_eq!(always.root.children[0].rect.width, 6);
    assert_eq!(always.root.children[0].children[0].rect.width, 5);

    scroll_view
        .borrow()
        .set_scrollbar(ScrollViewScrollbar::Hidden);
    assert_eq!(render().root.children[0].children[0].rect.width, 6);
    assert!(!scroll_view.borrow().is_scrollbar_visible());
}

#[test]
fn measures_nested_scroll_content_from_constrained_child_geometry() {
    let inner = Rc::new(RefCell::new(
        ScrollView::new(
            Box::new(Text::with_padding("1\n2\n3\n4\n5\n6", 0, 0)),
            ScrollViewOptions::default(),
        )
        .expect("valid scroll view"),
    ));
    let outer = Rc::new(RefCell::new(
        ScrollView::new(
            Box::new(VStack::new(
                vec![
                    StackChild::SharedEntry(
                        as_shared(&inner),
                        StackEntryOptions {
                            basis: Some(2.0),
                            ..Default::default()
                        },
                    ),
                    StackChild::Component(Box::new(Text::with_padding("tail", 0, 0))),
                ],
                StackOptions::default(),
            )),
            ScrollViewOptions::default(),
        )
        .expect("valid scroll view"),
    ));
    frame(as_shared(&outer), 10.0, 2.0);

    assert_eq!(inner.borrow().viewport_height(), 2);
    assert_eq!(outer.borrow().scroll_by(10.0), 9.0);
    assert_eq!(outer.borrow().scroll_top(), 1);
}

#[test]
fn rebuilds_geometry_after_content_changes() {
    let text = Rc::new(RefCell::new(Text::with_padding("one", 0, 0)));
    let root: SharedComponent = shared(VStack::new(
        vec![StackChild::Shared(as_shared(&text))],
        StackOptions::default(),
    ));
    let first = frame(Rc::clone(&root), 10.0, 4.0);
    text.borrow_mut().set_text("one\ntwo\nthree");
    let second = frame(Rc::clone(&root), 10.0, 4.0);

    assert_eq!(
        first.root.children[0]
            .lines
            .as_ref()
            .map(|lines| lines.len()),
        Some(1)
    );
    assert_eq!(
        second.root.children[0]
            .lines
            .as_ref()
            .map(|lines| lines.len()),
        Some(3)
    );
}

// ---------------------------------------------------------------------------
// 回归：瞬时滚动条的隐藏定时器到期时要请求重绘（reviewer finding 6）。
// TS 的 `setTimeout` 回调在隐藏滚动条后调用 `requestRenderCallback?.()`；
// Rust 侧在惰性结算到期时通过 `set_request_render` 安装的回调补发。
// ---------------------------------------------------------------------------

#[test]
fn requests_a_repaint_when_the_transient_scrollbar_hide_deadline_expires() {
    let renders = Rc::new(Cell::new(0));
    let scroll_view = ScrollView::new(
        Box::new(Text::with_padding("1\n2\n3\n4\n5\n6\n7\n8\n9\n10", 0, 0)),
        ScrollViewOptions {
            scrollbar: Some(ScrollViewScrollbar::Auto),
            scrollbar_hide_delay_ms: Some(10.0),
            ..Default::default()
        },
    )
    .expect("valid scroll view");
    let sink = Rc::clone(&renders);
    scroll_view.set_request_render(Some(Rc::new(move || sink.set(sink.get() + 1))));
    scroll_view.update_layout(10, 3, &|| {});
    assert_eq!(renders.get(), 0);

    scroll_view.scroll_by(1.0);
    assert_eq!(renders.get(), 1, "scroll operation requests a render");
    assert!(scroll_view.is_scrollbar_visible());

    std::thread::sleep(Duration::from_millis(30));
    assert!(!scroll_view.is_scrollbar_visible());
    assert_eq!(
        renders.get(),
        2,
        "TS hide timer requests one more render when it hides the scrollbar"
    );
}

// ---------------------------------------------------------------------------
// 回归（critical）：Stack/ScrollView 的共享句柄代理必须对焦点遍历透明。
//
// TS 的 `Stack extends Container`，`setFocus(editor)`、`containsComponent` 与鼠标焦点
// 解析都能穿过**任意深度**嵌套的 Stack/ScrollView（`tui.ts:738-746`, `2038`）。Rust 的
// 子项是共享句柄代理（`SharedHandle`），代理自身不是容器；它必须把 `container_children`
// 的遍历透传给底层组件，否则第二层 Stack 之后的目标无法从地址解析回路径，键盘输入会被
// 静默丢弃（reviewer finding：critical）。
// ---------------------------------------------------------------------------

#[path = "common/mod.rs"]
mod common;

use common::TestTerminal;
use kiana_tui::tui::{
    FocusTarget, Focusable, MouseFocusTarget, TuiBase, TuiMouseButton, TuiMouseEvent,
    TuiMouseEventResult, TuiMouseEventType, dispatch_mouse_event,
};

/// `focused` 标志的共享句柄。
type FocusFlag = Rc<Cell<bool>>;
/// 输入记录。
type InputLog = Rc<RefCell<Vec<String>>>;

/// 请求焦点的输入探针：渲染一行、记录键盘输入与 `focused` 标志。
struct FocusProbe {
    focused: FocusFlag,
    inputs: InputLog,
}

impl FocusProbe {
    fn new() -> (Self, FocusFlag, InputLog) {
        let focused = Rc::new(Cell::new(false));
        let inputs = Rc::new(RefCell::new(Vec::new()));
        (
            FocusProbe {
                focused: Rc::clone(&focused),
                inputs: Rc::clone(&inputs),
            },
            focused,
            inputs,
        )
    }
}

impl Component for FocusProbe {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec!["probe".to_string()]
    }

    fn handle_input(&mut self, data: &str) {
        self.inputs.borrow_mut().push(data.to_string());
    }

    fn handle_mouse(&mut self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        Some(TuiMouseEventResult {
            focus: Some(true),
            ..TuiMouseEventResult::default()
        })
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for FocusProbe {
    fn focused(&self) -> bool {
        self.focused.get()
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused.set(focused);
    }
}

fn press_at(y: isize, height: usize) -> TuiMouseEvent {
    TuiMouseEvent {
        event_type: TuiMouseEventType::Press,
        button: TuiMouseButton::Left,
        x: 0,
        y,
        screen_x: 0,
        screen_y: y,
        width: 20,
        height,
        shift: false,
        alt: false,
        ctrl: false,
        wheel_delta: None,
        click_count: None,
    }
}

#[test]
fn focus_paths_traverse_containers_nested_two_stack_levels_deep() {
    // 形状与聊天视口一致：TUI -> root VStack -> dock VStack -> Input。
    let (probe, focused, inputs) = FocusProbe::new();
    let dock = VStack::new(
        vec![StackChild::Component(Box::new(probe))],
        StackOptions::default(),
    );
    let root = VStack::new(
        vec![StackChild::Component(Box::new(dock))],
        StackOptions::default(),
    );

    let mut base = TuiBase::new(Box::new(TestTerminal::with_size(80, 24)), None, None);
    base.add_child(Box::new(root));

    // 鼠标分发：事件从根 VStack 逐层下钻到探针，返回的是探针句柄的地址。
    let event = press_at(0, 4);
    let result =
        dispatch_mouse_event(base.children[0].as_mut(), &event).expect("probe handles the press");
    let address = match result.focus_target {
        Some(MouseFocusTarget::Address(address)) => address,
        other => panic!("expected an address focus target, got {other:?}"),
    };

    // 地址必须解析回三层路径（TS 的引用身份等价物），而不是 None。
    let target = base
        .focus_target_of_address(address)
        .expect("address must resolve through nested stacks");
    assert_eq!(
        target,
        FocusTarget::Path {
            path: kiana_tui::tui::FocusPath::new(&[0, 0, 0]).expect("non-empty path")
        }
    );

    // setFocus 必须把 focused 写穿两层代理。
    base.set_focus(Some(target));
    assert!(
        focused.get(),
        "input must be marked focused through proxies"
    );

    // 键盘输入必须沿同一条路径到达输入组件（修复前会被静默丢弃）。
    base.handle_terminal_input("x");
    assert_eq!(
        inputs.borrow().as_slice(),
        ["x".to_string()],
        "keyboard input must reach the nested input"
    );
}

#[test]
fn focus_paths_traverse_a_scroll_view_nested_in_a_stack() {
    // ScrollView 也继承 Container；它的子项同样是共享句柄代理，遍历必须透明。
    let (probe, focused, inputs) = FocusProbe::new();
    let scroll =
        ScrollView::new(Box::new(probe), ScrollViewOptions::default()).expect("valid scroll view");
    let root = VStack::new(
        vec![StackChild::Component(Box::new(scroll))],
        StackOptions::default(),
    );

    let mut base = TuiBase::new(Box::new(TestTerminal::with_size(80, 24)), None, None);
    base.add_child(Box::new(root));

    let event = press_at(0, 4);
    let result =
        dispatch_mouse_event(base.children[0].as_mut(), &event).expect("probe handles the press");
    let address = match result.focus_target {
        Some(MouseFocusTarget::Address(address)) => address,
        other => panic!("expected an address focus target, got {other:?}"),
    };
    let target = base
        .focus_target_of_address(address)
        .expect("address must resolve through a nested ScrollView");
    assert_eq!(
        target,
        FocusTarget::Path {
            path: kiana_tui::tui::FocusPath::new(&[0, 0, 0]).expect("non-empty path")
        }
    );

    base.set_focus(Some(target));
    assert!(focused.get());
    base.handle_terminal_input("x");
    assert_eq!(inputs.borrow().as_slice(), ["x".to_string()]);
}

// ---------------------------------------------------------------------------
// 回归：`scrollBy` 的算术必须保持 f64（TS number），不能溢出 `i64`。
// ---------------------------------------------------------------------------

#[test]
fn scroll_by_handles_very_large_finite_deltas_without_overflow() {
    let scroll_view = ScrollView::new(
        Box::new(Text::with_padding("1\n2\n3\n4\n5\n6\n7\n8\n9\n10", 0, 0)),
        ScrollViewOptions::default(),
    )
    .expect("valid scroll view");
    scroll_view.update_layout(10, 3, &|| {});
    scroll_view.scroll_to(3.0, Default::default());
    assert_eq!(scroll_view.scroll_top(), 3);

    // TS：next 夹到 maxScrollTop（10 - 3 = 7），返回 requested - moved ≈ 1e300。
    let unused = scroll_view.scroll_by(1e300);
    assert_eq!(scroll_view.scroll_top(), 7);
    assert_eq!(unused, 1e300);
}

// ---------------------------------------------------------------------------
// 回归：极端/非有限 `scrollbarHideDelayMs` 不能 panic。
// TS 只是把值交给 `setTimeout`（Node 对越界值按 1 ms 排程），绝不会抛。
// ---------------------------------------------------------------------------

#[test]
fn extreme_scrollbar_hide_delays_do_not_panic() {
    for delay in [1e300_f64, f64::INFINITY, f64::NAN, 0.0, 0.5] {
        let scroll_view = ScrollView::new(
            Box::new(Text::with_padding("1\n2\n3\n4\n5\n6\n7\n8\n9\n10", 0, 0)),
            ScrollViewOptions {
                scrollbar: Some(ScrollViewScrollbar::Auto),
                scrollbar_hide_delay_ms: Some(delay),
                ..Default::default()
            },
        )
        .expect("valid scroll view");
        scroll_view.update_layout(10, 3, &|| {});
        scroll_view.scroll_by(1.0);
        assert!(
            scroll_view.is_scrollbar_visible(),
            "scrollbar stays visible right after scrolling (delay {delay})"
        );
    }
}
