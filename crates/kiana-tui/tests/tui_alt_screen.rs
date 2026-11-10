//! `test/tui-alt-screen.test.ts` 的移植（68 个 it；逐条对应，见 `spec/tui-core.md` §7.3）。
//!
//! ## 与 TS 测试的表示差异
//!
//! TS 用 `VirtualTerminal`（`@xterm/headless`）驱动异步渲染循环；Rust 的
//! [`TestTerminal`] 只捕获写入字节流，没有 VT 仿真器。因此：
//!
//! - "视口内容"改读 [`TuiAltScreen::get_screen_lines`]（最后一帧写出的行），去掉 ANSI 与
//!   行尾 padding 后与 TS 的 `getViewport().map(line => line.trimEnd())` 逐行比较；
//! - 输入直接调用 [`TuiAltScreen::handle_terminal_input`]（等价 `terminal.sendInput`），
//!   每步之后显式 `render_now(false)`（等价 `await terminal.waitForRender()`）；
//! - 需要观察写入序列（OSC 52、Kitty 图形、鼠标模式）时读取 `TestTerminal` 的输出缓冲；
//! - 定时器相关的用例（flash 到期、自动滚动条隐藏、选择自动滚动）用 `std::thread::sleep`
//!   驱动惰性定时器（见 `src/flash.rs` 与 `advance_selection_auto_scroll` 的文档）。
//!
//! 组件必须在测试里继续可变时，用 `Rc<RefCell<…>>` + [`TuiAltScreen::add_child_shared`]
//! 或 `StackChild::Shared`（TS 的 `const text = new Text(...); tui.addChild(text)` 语义）。

#[path = "common/mod.rs"]
mod common;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use common::TestTerminal;
use kiana_tui::alt_screen::{
    CopySelectionDispatch, CopySelectionDone, CopySelectionResult, CopySelectionStatus,
    TuiAltScreen, TuiAltScreenOptions,
};
use kiana_tui::components::box_component::BoxComponent;
use kiana_tui::components::h_stack::HStack;
use kiana_tui::components::mouse_region::MouseRegion;
use kiana_tui::components::scroll_view::{
    ScrollView, ScrollViewFollow, ScrollViewOptions, ScrollViewScrollbar,
};
use kiana_tui::components::select_list::{
    SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme,
};
use kiana_tui::components::stack::{StackChild, StackEntry, StackEntryOptions, StackOptions};
use kiana_tui::components::text_component::Text;
use kiana_tui::components::v_stack::VStack;
use kiana_tui::layout_node::SharedComponent;
use kiana_tui::text::{strip_terminal_sequences, visible_width};
use kiana_tui::tui::{
    Component, Container, FocusTarget, Focusable, OverlayAnchor, OverlayOptions, SizeValue, Tui,
    TuiMode, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType, TuiStopOptions,
};

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

/// `new TuiAltScreen(terminal, undefined, undefined, options)`。
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

/// 去掉 ANSI 与行尾空白后的可见行（TS `getViewport().map(l => l.trimEnd())`）。
fn screen(tui: &TuiAltScreen) -> Vec<String> {
    tui.get_screen_lines()
        .iter()
        .map(|line| strip_terminal_sequences(line).trim_end().to_string())
        .collect()
}

/// 去掉 ANSI 但**保留**行尾 padding（TS `getViewport()` 的等价物）。
fn padded_screen(tui: &TuiAltScreen) -> Vec<String> {
    tui.get_screen_lines()
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect()
}

fn stop(tui: &mut TuiAltScreen) {
    tui.stop(TuiStopOptions::default());
}

/// TS `new Text(text, 0, 0)`（无留白）。
fn text(value: impl Into<String>) -> Text {
    Text::with_padding(value, 0, 0)
}

fn shared<T: 'static>(value: T) -> Rc<RefCell<T>> {
    Rc::new(RefCell::new(value))
}

fn as_shared<T: Component + 'static>(value: &Rc<RefCell<T>>) -> SharedComponent {
    value.clone()
}

fn stack_entry(component: SharedComponent, options: StackEntryOptions) -> StackChild {
    StackChild::SharedEntry(component, options)
}

/// 把共享组件句柄装进 `Box<dyn Component>`（布局根的构造用）。
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

/// 记录键盘输入的编辑器替身（TS 测试里的 `{ render: () => ["editor"], … }`）。
struct RecordingEditor {
    inputs: Rc<RefCell<Vec<String>>>,
}

impl Component for RecordingEditor {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec!["editor".to_string()]
    }
    fn handle_input(&mut self, data: &str) {
        self.inputs.borrow_mut().push(data.to_string());
    }
    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for RecordingEditor {
    fn focused(&self) -> bool {
        false
    }
    fn set_focused(&mut self, _focused: bool) {}
}

/// 只记录输入的 overlay（TS `InputOverlay`）。
struct InputOverlay {
    inputs: Rc<RefCell<Vec<String>>>,
    focused: Rc<Cell<bool>>,
}

impl InputOverlay {
    fn new() -> Self {
        InputOverlay {
            inputs: Rc::new(RefCell::new(Vec::new())),
            focused: Rc::new(Cell::new(false)),
        }
    }
}

impl Component for InputOverlay {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec!["overlay".to_string()]
    }
    fn handle_input(&mut self, data: &str) {
        self.inputs.borrow_mut().push(data.to_string());
    }
    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for InputOverlay {
    fn focused(&self) -> bool {
        self.focused.get()
    }
    fn set_focused(&mut self, focused: bool) {
        self.focused.set(focused);
    }
}

fn focus_shared(tui: &mut TuiAltScreen, component: &SharedComponent) -> FocusTarget {
    let target = tui
        .focus_target_of_component(component)
        .expect("component is mounted");
    tui.set_focus(Some(target));
    target
}

fn assert_screen(tui: &TuiAltScreen, expected: &[&str]) {
    let actual = screen(tui);
    let expected: Vec<String> = expected.iter().map(|line| (*line).to_string()).collect();
    assert_eq!(actual, expected, "screen mismatch");
}

// ---------------------------------------------------------------------------
// 视口与滚动（1-7）
// ---------------------------------------------------------------------------

/// 1 `renders a terminal-height viewport and preserves manual scroll position`
#[test]
fn renders_a_terminal_height_viewport_and_preserves_manual_scroll_position() {
    let (mut tui, _) = alt(20, 4);
    let content = shared(text(
        (1..=10)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    ));
    tui.add_child_shared(as_shared(&content));
    tui.start();
    tui.render_now(false);

    assert_screen(&tui, &["line 7", "line 8", "line 9", "line 10"]);
    assert!(tui.is_following_output());

    tui.handle_terminal_input("\x1b[<64;1;1M");
    tui.render_now(false);
    assert_screen(&tui, &["line 6", "line 7", "line 8", "line 9"]);
    assert_eq!(tui.viewport_top(), 5);
    assert!(!tui.is_following_output());

    content.borrow_mut().set_text(
        (1..=12)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    tui.request_render(false);
    tui.render_now(false);
    assert_screen(&tui, &["line 6", "line 7", "line 8", "line 9"]);
    stop(&mut tui);
}

/// 2 `shows a clickable jump-to-end indicator on the transcript's last row while scrolled up`
#[test]
fn shows_a_clickable_jump_to_end_indicator_on_the_transcripts_last_row_while_scrolled_up() {
    let (mut tui, _) = alt_with(
        30,
        6,
        TuiAltScreenOptions {
            scroll_to_end_indicator: Some(std::rc::Rc::new(|| {
                "\x1b[7m ↓ Jump to end \x1b[27m".into()
            })),
            ..Default::default()
        },
    );
    let transcript = shared(
        ScrollView::new(
            Box::new(text(
                (1..=8)
                    .map(|index| format!("line {index}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
            ScrollViewOptions {
                follow: Some(ScrollViewFollow::End),
                primary: Some(true),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let root = VStack::new(
        vec![
            stack_entry(
                as_shared(&transcript),
                StackEntryOptions {
                    basis: Some(0.0),
                    grow: Some(1.0),
                    min_size: Some(1.0),
                    ..Default::default()
                },
            ),
            StackChild::Component(Box::new(text("editor\nfooter"))),
        ],
        StackOptions::default(),
    );
    tui.set_layout_root_component(Some(Box::new(root)));
    tui.start();
    tui.render_now(false);
    assert!(!screen(&tui).iter().any(|line| line.contains("Jump to end")));

    tui.handle_terminal_input("\x1b[<64;1;1M");
    tui.render_now(false);
    assert!(!transcript.borrow().is_following_end());
    assert_eq!(screen(&tui)[3], "line 7  ↓ Jump to end");
    assert_eq!(screen(&tui)[4], "editor");

    // 按在标签旁边会开始选择，而不是跳到底部。
    tui.handle_terminal_input("\x1b[<0;2;4M");
    tui.handle_terminal_input("\x1b[<0;2;4m");
    tui.render_now(false);
    assert!(!transcript.borrow().is_following_end());

    tui.handle_terminal_input("\x1b[<0;15;4M");
    tui.handle_terminal_input("\x1b[<0;15;4m");
    tui.render_now(false);
    assert!(transcript.borrow().is_following_end());
    assert_screen(
        &tui,
        &["line 5", "line 6", "line 7", "line 8", "editor", "footer"],
    );
    stop(&mut tui);
}

/// 3 `keeps the jump-to-end indicator centered as the auto scrollbar hides and reappears`
#[test]
fn keeps_the_jump_to_end_indicator_centered_as_the_auto_scrollbar_hides_and_reappears() {
    let label = " ↓ Jump to latest message · End ";
    let (mut tui, _) = alt_with(
        80,
        6,
        TuiAltScreenOptions {
            scroll_to_end_indicator: Some(std::rc::Rc::new(move || label.to_string())),
            ..Default::default()
        },
    );
    let transcript = shared(
        ScrollView::new(
            Box::new(text(
                (1..=20)
                    .map(|index| format!("line {index}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
            ScrollViewOptions {
                follow: Some(ScrollViewFollow::End),
                primary: Some(true),
                scrollbar: Some(ScrollViewScrollbar::Auto),
                scrollbar_hide_delay_ms: Some(0.0),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let root: SharedComponent = as_shared(&transcript);
    tui.set_layout_root_component(Some(Box::new(SharedRef(root))));
    tui.start();
    tui.render_now(false);

    // 指针停在滚动条上时保持可见，直到指针离开。
    tui.handle_terminal_input("\x1b[<64;80;1M");
    tui.render_now(false);
    assert!(transcript.borrow().is_scrollbar_visible());
    assert!(!transcript.borrow().is_following_end());
    let scroll_top = transcript.borrow().scroll_top();
    let visible_column = padded_screen(&tui)[5].find(label).expect("label visible");

    // 离开滚动条后自动隐藏定时器到期，内容不变。
    tui.handle_terminal_input("\x1b[<35;79;1M");
    std::thread::sleep(Duration::from_millis(10));
    tui.render_now(false);
    assert!(!transcript.borrow().is_scrollbar_visible());
    assert_eq!(transcript.borrow().scroll_top(), scroll_top);
    let hidden_column = padded_screen(&tui)[5].find(label).expect("label visible");

    tui.handle_terminal_input("\x1b[<35;80;1M");
    tui.render_now(false);
    assert!(transcript.borrow().is_scrollbar_visible());
    assert_eq!(transcript.borrow().scroll_top(), scroll_top);
    let revealed_column = padded_screen(&tui)[5].find(label).expect("label visible");

    assert_eq!(
        [visible_column, hidden_column, revealed_column],
        [24, 24, 24]
    );
    stop(&mut tui);
}

/// 4 `leaves the scrollbar visible and clickable when the jump-to-end indicator spans the transcript`
#[test]
fn leaves_the_scrollbar_visible_and_clickable_when_the_jump_to_end_indicator_spans_the_transcript()
{
    let (mut tui, _) = alt_with(
        30,
        6,
        TuiAltScreenOptions {
            scroll_to_end_indicator: Some(std::rc::Rc::new(|| "↓".repeat(30))),
            ..Default::default()
        },
    );
    let transcript = shared(
        ScrollView::new(
            Box::new(text(
                (1..=12)
                    .map(|index| format!("line {index}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
            ScrollViewOptions {
                follow: Some(ScrollViewFollow::End),
                primary: Some(true),
                scrollbar: Some(ScrollViewScrollbar::Always),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let root = VStack::new(
        vec![
            stack_entry(
                as_shared(&transcript),
                StackEntryOptions {
                    basis: Some(0.0),
                    grow: Some(1.0),
                    min_size: Some(1.0),
                    ..Default::default()
                },
            ),
            StackChild::Component(Box::new(text("editor\nfooter"))),
        ],
        StackOptions::default(),
    );
    tui.set_layout_root_component(Some(Box::new(root)));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<64;1;1M");
    tui.render_now(false);
    assert!(!transcript.borrow().is_following_end());

    // 指示器不得拦截滚动条最后一列上的按下。
    assert_eq!(screen(&tui)[3], format!("{}┃", "↓".repeat(29)));
    tui.handle_terminal_input("\x1b[<0;30;4M");
    tui.handle_terminal_input("\x1b[<0;30;4m");
    tui.render_now(false);
    assert!(!transcript.borrow().is_following_end());
    stop(&mut tui);
}

/// 5 `never shows the jump-to-end indicator for a primary scroll view without follow-end`
#[test]
fn never_shows_the_jump_to_end_indicator_for_a_primary_scroll_view_without_follow_end() {
    let (mut tui, _) = alt_with(
        30,
        3,
        TuiAltScreenOptions {
            scroll_to_end_indicator: Some(std::rc::Rc::new(|| " ↓ Jump to end ".into())),
            ..Default::default()
        },
    );
    let transcript = shared(
        ScrollView::new(
            Box::new(text("one\ntwo\nthree\nfour\nfive")),
            ScrollViewOptions {
                primary: Some(true),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    tui.set_layout_root_component(Some(Box::new(SharedRef(as_shared(&transcript)))));
    tui.start();
    tui.render_now(false);

    assert!(!transcript.borrow().is_following_end());
    assert!(!screen(&tui).iter().any(|line| line.contains("Jump to end")));
    stop(&mut tui);
}

/// 6 `keeps an explicit dock fixed while the transcript scrolls`
#[test]
fn keeps_an_explicit_dock_fixed_while_the_transcript_scrolls() {
    let (mut tui, _) = alt(20, 6);
    let transcript_text = shared(text(
        (1..=8)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    ));
    let transcript = shared(
        ScrollView::new(
            Box::new(SharedRef(as_shared(&transcript_text))),
            ScrollViewOptions {
                follow: Some(ScrollViewFollow::End),
                primary: Some(true),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let dock = VStack::new(
        vec![
            StackChild::Component(Box::new(text("editor"))),
            StackChild::Component(Box::new(text("footer"))),
        ],
        StackOptions::default(),
    );
    let root = VStack::new(
        vec![
            stack_entry(
                as_shared(&transcript),
                StackEntryOptions {
                    basis: Some(0.0),
                    grow: Some(1.0),
                    min_size: Some(1.0),
                    ..Default::default()
                },
            ),
            StackChild::Component(Box::new(dock)),
        ],
        StackOptions::default(),
    );
    tui.set_layout_root_component(Some(Box::new(root)));
    tui.start();
    tui.render_now(false);
    assert_screen(
        &tui,
        &["line 5", "line 6", "line 7", "line 8", "editor", "footer"],
    );

    // 指针在 dock 上滚轮会退回主转录滚动视图。
    tui.handle_terminal_input("\x1b[<64;1;6M");
    tui.render_now(false);
    assert_screen(
        &tui,
        &["line 4", "line 5", "line 6", "line 7", "editor", "footer"],
    );
    assert!(!transcript.borrow().is_following_end());

    transcript_text.borrow_mut().set_text(
        (1..=10)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    tui.request_render(false);
    tui.render_now(false);
    assert_screen(
        &tui,
        &["line 4", "line 5", "line 6", "line 7", "editor", "footer"],
    );

    tui.scroll_to_bottom();
    tui.render_now(false);
    assert_screen(
        &tui,
        &["line 7", "line 8", "line 9", "line 10", "editor", "footer"],
    );
    stop(&mut tui);
}

/// 7 `invalidates overlays with an explicit layout root`
#[test]
fn invalidates_overlays_with_an_explicit_layout_root() {
    let (mut tui, _) = alt(20, 4);
    let invalidated = Rc::new(Cell::new(false));
    struct CountingOverlay {
        invalidated: Rc<Cell<bool>>,
    }
    impl Component for CountingOverlay {
        fn render(&mut self, _width: usize) -> Vec<String> {
            vec!["overlay".to_string()]
        }
        fn invalidate(&mut self) {
            self.invalidated.set(true);
        }
    }
    tui.set_layout_root_component(Some(Box::new(text("root"))));
    tui.show_overlay(
        Box::new(CountingOverlay {
            invalidated: Rc::clone(&invalidated),
        }),
        Default::default(),
    );
    tui.invalidate();
    assert!(invalidated.get());
    stop(&mut tui);
}

// ---------------------------------------------------------------------------
// 滚轮路由（8-16）
// ---------------------------------------------------------------------------

/// 8 `routes wheel input to the scroll view under the pointer`
#[test]
fn routes_wheel_input_to_the_scroll_view_under_the_pointer() {
    let (mut tui, _) = alt(20, 4);
    let left = shared(
        ScrollView::new(
            Box::new(text("a1\na2\na3\na4\na5\na6\na7")),
            ScrollViewOptions {
                follow: Some(ScrollViewFollow::End),
                primary: Some(true),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let right = shared(
        ScrollView::new(
            Box::new(text("b1\nb2\nb3\nb4\nb5\nb6\nb7")),
            ScrollViewOptions {
                follow: Some(ScrollViewFollow::End),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let root = HStack::new(
        vec![
            stack_entry(
                as_shared(&left),
                StackEntryOptions {
                    basis: Some(10.0),
                    shrink: Some(0.0),
                    ..Default::default()
                },
            ),
            stack_entry(
                as_shared(&right),
                StackEntryOptions {
                    basis: Some(10.0),
                    shrink: Some(0.0),
                    ..Default::default()
                },
            ),
        ],
        StackOptions::default(),
    );
    tui.set_layout_root_component(Some(Box::new(root)));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<64;15;1M");
    tui.render_now(false);
    assert_eq!(left.borrow().scroll_top(), 3);
    assert_eq!(right.borrow().scroll_top(), 2);
    assert_screen(
        &tui,
        &[
            "a4        b3",
            "a5        b4",
            "a6        b5",
            "a7        b6",
        ],
    );
    stop(&mut tui);
}

/// 9 `scrolls faster while Alt is held during wheel input`
#[test]
fn scrolls_faster_while_alt_is_held_during_wheel_input() {
    let (mut tui, _) = alt(20, 4);
    tui.add_child(Box::new(text(
        (1..=12)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )));
    tui.start();
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 8);

    // Alt 修饰键在滚轮按键码上置 bit 8（72 = 64 + 8）。
    tui.handle_terminal_input("\x1b[<72;1;1M");
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 3);
    stop(&mut tui);
}

/// 10 `does not vertically redispatch misses through horizontal layout containers`
#[test]
fn does_not_vertically_redispatch_misses_through_horizontal_layout_containers() {
    let (mut tui, _) = alt(20, 2);
    let selections = Rc::new(Cell::new(0usize));
    let list = shared(SelectList::new(
        vec![
            SelectItem::with_label("a", "A"),
            SelectItem::with_label("b", "B"),
        ],
        2,
        SelectListTheme::plain(),
        SelectListLayoutOptions::default(),
    ));
    let selections_clone = Rc::clone(&selections);
    list.borrow_mut().on_select = Some(Box::new(move |_item| {
        selections_clone.set(selections_clone.get() + 1)
    }));
    let root = HStack::new(
        vec![
            stack_entry(
                as_shared(&list),
                StackEntryOptions {
                    basis: Some(10.0),
                    ..Default::default()
                },
            ),
            stack_entry(
                {
                    let plain = shared(text("plain"));
                    as_shared(&plain)
                },
                StackEntryOptions {
                    basis: Some(10.0),
                    ..Default::default()
                },
            ),
        ],
        StackOptions::default(),
    );
    tui.set_layout_root_component(Some(Box::new(root)));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;15;1M");
    tui.handle_terminal_input("\x1b[<0;15;1m");
    tui.render_now(false);
    assert_eq!(selections.get(), 0);
    stop(&mut tui);
}

/// 串行化依赖环境变量/全局键位的用例（Rust 测试默认并行）。
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn env_snapshot(keys: &[&str]) -> Vec<(String, Option<String>)> {
    keys.iter()
        .map(|key| ((*key).to_string(), std::env::var(key).ok()))
        .collect()
}

fn env_restore(saved: Vec<(String, Option<String>)>) {
    for (key, value) in saved {
        match value {
            Some(value) => unsafe { std::env::set_var(&key, value) },
            None => unsafe { std::env::remove_var(&key) },
        }
    }
}

/// 11 `uses button-motion tracking inside terminal multiplexers`
#[test]
fn uses_button_motion_tracking_inside_terminal_multiplexers() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let keys = ["TMUX", "ZELLIJ", "STY", "TERM"];
    let saved = env_snapshot(&keys);
    for key in keys {
        unsafe { std::env::remove_var(key) };
    }
    unsafe { std::env::set_var("TERM", "xterm-256color") };
    let (mut direct_tui, direct_output) = alt(20, 4);
    direct_tui.start();
    let direct_writes = direct_output.borrow().clone();
    assert!(direct_writes.contains("\x1b[?1003h"));
    stop(&mut direct_tui);

    let multiplexers: Vec<Vec<(&str, &str)>> = vec![
        vec![("TMUX", "/tmp/tmux/default,1,0")],
        vec![("TERM", "tmux-256color")],
        vec![("ZELLIJ", "0")],
        vec![("STY", "123.session")],
        vec![("TERM", "screen-256color")],
    ];
    for environment in multiplexers {
        for key in keys {
            unsafe { std::env::remove_var(key) };
        }
        for (key, value) in &environment {
            unsafe { std::env::set_var(key, value) };
        }
        let (mut tui, output) = alt(20, 4);
        tui.start();
        let writes = output.borrow().clone();
        let name = format!("{environment:?}");
        assert!(
            writes.contains("\x1b[?1002h"),
            "{name} should enable button-motion tracking"
        );
        assert!(
            !writes.contains("\x1b[?1003h"),
            "{name} should not enable all-motion tracking"
        );
        assert!(
            writes.contains("\x1b[?1006h"),
            "{name} should enable SGR mouse encoding"
        );
        stop(&mut tui);
    }
    env_restore(saved);
}

/// 12 `invokes the right-click paste handler only on Windows outside VS Code`
#[test]
fn invokes_the_right_click_paste_handler_only_on_windows_outside_vs_code() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let saved = env_snapshot(&["TERM_PROGRAM"]);
    let paste_count = Rc::new(Cell::new(0usize));
    let counter = Rc::clone(&paste_count);
    let (mut tui, _) = alt_with(
        20,
        4,
        TuiAltScreenOptions {
            on_right_click_paste: Some(std::rc::Rc::new(move || {
                counter.set(counter.get() + 1);
            })),
            ..Default::default()
        },
    );
    unsafe { std::env::remove_var("TERM_PROGRAM") };
    tui.set_platform_override(Some("win32"));
    tui.start();
    tui.handle_terminal_input("\x1b[<2;1;1M");
    tui.handle_terminal_input("\x1b[<2;1;1m");
    assert_eq!(paste_count.get(), 1);

    unsafe { std::env::set_var("TERM_PROGRAM", "vscode") };
    tui.handle_terminal_input("\x1b[<2;1;1M");
    assert_eq!(paste_count.get(), 1);

    tui.set_platform_override(Some("linux"));
    unsafe { std::env::remove_var("TERM_PROGRAM") };
    tui.handle_terminal_input("\x1b[<2;1;1M");
    assert_eq!(paste_count.get(), 1);
    stop(&mut tui);
    env_restore(saved);
}

/// 13 `reveals an auto scrollbar when the pointer enters its hidden track`
#[test]
fn reveals_an_auto_scrollbar_when_the_pointer_enters_its_hidden_track() {
    let (mut tui, output) = alt(10, 5);
    let scroll_view = shared(
        ScrollView::new(
            Box::new(text(
                (1..=20)
                    .map(|index| format!("line {index}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
            ScrollViewOptions {
                primary: Some(true),
                scrollbar: Some(ScrollViewScrollbar::Auto),
                scrollbar_hide_delay_ms: Some(20.0),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    tui.set_layout_root_component(Some(Box::new(SharedRef(as_shared(&scroll_view)))));
    tui.start();
    tui.render_now(false);
    assert!(!scroll_view.borrow().is_scrollbar_visible());

    tui.handle_terminal_input("\x1b[<35;10;3M");
    tui.render_now(false);
    assert!(scroll_view.borrow().is_scrollbar_visible());
    assert!(scroll_view.borrow().is_scrollbar_active());
    assert!(
        padded_screen(&tui)
            .iter()
            .any(|line| line.contains('│') || line.contains('█'))
    );

    tui.handle_terminal_input("\x1b[<35;9;3M");
    std::thread::sleep(Duration::from_millis(40));
    tui.render_now(false);
    assert!(!scroll_view.borrow().is_scrollbar_visible());
    let _ = output;
    stop(&mut tui);
}

/// 14 `jumps to a scrollbar track position and continues dragging from there`
#[test]
fn jumps_to_a_scrollbar_track_position_and_continues_dragging_from_there() {
    let (mut tui, output) = alt(10, 10);
    let scroll_view = shared(
        ScrollView::new(
            Box::new(text(
                (1..=50)
                    .map(|index| format!("line {index}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
            ScrollViewOptions {
                primary: Some(true),
                scrollbar: Some(ScrollViewScrollbar::Always),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    tui.set_layout_root_component(Some(Box::new(SharedRef(as_shared(&scroll_view)))));
    tui.start();
    tui.render_now(false);
    assert_eq!(scroll_view.borrow().scroll_top(), 0);

    tui.handle_terminal_input("\x1b[<0;10;6M");
    tui.render_now(false);
    assert_eq!(scroll_view.borrow().scroll_top(), 20);

    tui.handle_terminal_input("\x1b[<32;10;10M");
    tui.render_now(false);
    assert_eq!(scroll_view.borrow().scroll_top(), 40);

    tui.handle_terminal_input("\x1b[<0;10;10m");
    tui.render_now(false);
    assert!(!output.borrow().contains("\x1b]52;c;"));
    stop(&mut tui);
}

/// 15 `applies runtime wheel line count updates`
#[test]
fn applies_runtime_wheel_line_count_updates() {
    let (mut tui, _) = alt_with(
        20,
        4,
        TuiAltScreenOptions {
            wheel_scroll_lines: Some(kiana_tui::wheel::WheelScrollLines::Fixed(3.0)),
            ..Default::default()
        },
    );
    let deltas: Rc<RefCell<Vec<Option<f64>>>> = Rc::new(RefCell::new(Vec::new()));
    let collected = Rc::clone(&deltas);
    tui.add_child(Box::new(MouseRegion::new(
        Box::new(text("wheel target")),
        Box::new(move |event: &TuiMouseEvent| {
            if event.event_type != TuiMouseEventType::Wheel {
                return None;
            }
            collected.borrow_mut().push(event.wheel_delta);
            Some(TuiMouseEventResult {
                handled: Some(true),
                ..Default::default()
            })
        }),
    )));
    tui.start();
    tui.render_now(false);
    tui.handle_terminal_input("\x1b[<64;1;1M");
    tui.set_wheel_scroll_lines(kiana_tui::wheel::WheelScrollLines::Fixed(2.0));
    tui.handle_terminal_input("\x1b[<65;1;1M");
    tui.handle_terminal_input("\x1b[<72;1;1M");
    assert_eq!(*deltas.borrow(), vec![Some(-3.0), Some(2.0), Some(-10.0)]);
    stop(&mut tui);
}

/// 16 `chains unused wheel delta to an outer scroll view`
#[test]
fn chains_unused_wheel_delta_to_an_outer_scroll_view() {
    let (mut tui, _) = alt_with(
        20,
        4,
        TuiAltScreenOptions {
            wheel_scroll_lines: Some(kiana_tui::wheel::WheelScrollLines::Fixed(3.0)),
            ..Default::default()
        },
    );
    let inner = shared(
        ScrollView::new(
            Box::new(text("i1\ni2\ni3\ni4\ni5\ni6")),
            ScrollViewOptions::default(),
        )
        .unwrap(),
    );
    let outer = shared(
        ScrollView::new(
            Box::new(VStack::new(
                vec![
                    stack_entry(
                        as_shared(&inner),
                        StackEntryOptions {
                            basis: Some(2.0),
                            ..Default::default()
                        },
                    ),
                    StackChild::Component(Box::new(text("tail1\ntail2\ntail3\ntail4\ntail5"))),
                ],
                StackOptions::default(),
            )),
            ScrollViewOptions {
                primary: Some(true),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    tui.set_layout_root_component(Some(Box::new(SharedRef(as_shared(&outer)))));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<65;1;1M");
    tui.render_now(false);
    assert_eq!(inner.borrow().scroll_top(), 3);
    assert_eq!(outer.borrow().scroll_top(), 0);

    tui.handle_terminal_input("\x1b[<65;1;1M");
    tui.render_now(false);
    assert_eq!(inner.borrow().scroll_top(), 4);
    assert_eq!(outer.borrow().scroll_top(), 2);
    stop(&mut tui);
}

/// 17 `supports configurable keyboard viewport navigation with four rows of page overlap`
#[test]
fn supports_configurable_keyboard_viewport_navigation_with_four_rows_of_page_overlap() {
    let (mut tui, _) = alt(20, 8);
    tui.add_child(Box::new(text(
        (1..=12)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[57421u");
    tui.handle_terminal_input("\x1b[57421;1:3u");
    tui.render_now(false);
    assert_screen(
        &tui,
        &[
            "line 1", "line 2", "line 3", "line 4", "line 5", "line 6", "line 7", "line 8",
        ],
    );

    tui.handle_terminal_input("\x1b[57422u");
    tui.handle_terminal_input("\x1b[57422;1:3u");
    tui.render_now(false);
    assert_screen(
        &tui,
        &[
            "line 5", "line 6", "line 7", "line 8", "line 9", "line 10", "line 11", "line 12",
        ],
    );

    tui.handle_terminal_input("\x1b[7^");
    tui.render_now(false);
    assert_screen(
        &tui,
        &[
            "line 1", "line 2", "line 3", "line 4", "line 5", "line 6", "line 7", "line 8",
        ],
    );

    tui.handle_terminal_input("\x1b[8^");
    tui.render_now(false);
    assert_screen(
        &tui,
        &[
            "line 5", "line 6", "line 7", "line 8", "line 9", "line 10", "line 11", "line 12",
        ],
    );
    stop(&mut tui);
}

/// 18 `scrolls the transcript by half a page with custom bindings`
#[test]
fn scrolls_the_transcript_by_half_a_page_with_custom_bindings() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let original = kiana_tui::keybindings::get_keybindings().clone();
    let mut config = kiana_tui::keybindings::KeybindingsConfig::new();
    config.insert("tui.altScreen.halfPageUp".to_string(), "ctrl+u".into());
    config.insert("tui.altScreen.halfPageDown".to_string(), "ctrl+d".into());
    kiana_tui::keybindings::set_keybindings(kiana_tui::keybindings::KeybindingsManager::new(
        kiana_tui::keybindings::tui_keybindings_static().clone(),
        config,
    ));
    let (mut tui, _) = alt(20, 10);
    tui.add_child(Box::new(text(
        (1..=30)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )));
    tui.start();
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 20);

    tui.handle_terminal_input("\x15");
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 15);

    tui.handle_terminal_input("\x04");
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 20);
    stop(&mut tui);
    kiana_tui::keybindings::set_keybindings(original);
}

/// 19 `scrolls the transcript by one line with custom bindings`
#[test]
fn scrolls_the_transcript_by_one_line_with_custom_bindings() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let original = kiana_tui::keybindings::get_keybindings().clone();
    let mut config = kiana_tui::keybindings::KeybindingsConfig::new();
    config.insert("tui.altScreen.lineUp".to_string(), "ctrl+y".into());
    config.insert("tui.altScreen.lineDown".to_string(), "ctrl+e".into());
    kiana_tui::keybindings::set_keybindings(kiana_tui::keybindings::KeybindingsManager::new(
        kiana_tui::keybindings::tui_keybindings_static().clone(),
        config,
    ));
    let (mut tui, _) = alt(20, 10);
    tui.add_child(Box::new(text(
        (1..=30)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )));
    tui.start();
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 20);

    tui.handle_terminal_input("\x19");
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 19);

    tui.handle_terminal_input("\x05");
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 20);
    stop(&mut tui);
    kiana_tui::keybindings::set_keybindings(original);
}

/// 20 `routes Home and End to the focused component and Ctrl+Home/End to the transcript`
#[test]
fn routes_home_and_end_to_the_focused_component_and_ctrl_home_end_to_the_transcript() {
    let (mut tui, _) = alt(20, 6);
    let transcript = shared(
        ScrollView::new(
            Box::new(text(
                (1..=12)
                    .map(|index| format!("line {index}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
            ScrollViewOptions {
                follow: Some(ScrollViewFollow::End),
                primary: Some(true),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let inputs: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let editor = shared(RecordingEditor {
        inputs: Rc::clone(&inputs),
    });
    let root = VStack::new(
        vec![
            stack_entry(
                as_shared(&transcript),
                StackEntryOptions {
                    basis: Some(0.0),
                    grow: Some(1.0),
                    min_size: Some(1.0),
                    ..Default::default()
                },
            ),
            stack_entry(
                as_shared(&editor),
                StackEntryOptions {
                    basis: Some(1.0),
                    shrink: Some(0.0),
                    ..Default::default()
                },
            ),
        ],
        StackOptions::default(),
    );
    tui.set_layout_root_component(Some(Box::new(root)));
    focus_shared(&mut tui, &as_shared(&editor));
    tui.start();
    tui.render_now(false);

    let bottom = transcript.borrow().scroll_top();
    assert!(bottom > 0);

    // #10314：未修饰的 Home/End 在任何 UI 模式下都属于编辑器。
    let editor_keys = ["\x1bOH", "\x1b[F", "\x1b[57423u", "\x1b[5;5~", "\x1b[6;5~"];
    for input in editor_keys {
        tui.handle_terminal_input(input);
    }
    tui.render_now(false);
    assert_eq!(transcript.borrow().scroll_top(), bottom);
    assert_eq!(
        *inputs.borrow(),
        editor_keys
            .iter()
            .map(|key| key.to_string())
            .collect::<Vec<_>>()
    );

    tui.handle_terminal_input("\x1b[1;5H");
    tui.render_now(false);
    assert_eq!(transcript.borrow().scroll_top(), 0);

    tui.handle_terminal_input("\x1b[1;5F");
    tui.render_now(false);
    assert_eq!(transcript.borrow().scroll_top(), bottom);
    assert!(transcript.borrow().is_following_end());

    tui.handle_terminal_input("\x1b[57423;5u");
    tui.handle_terminal_input("\x1b[57423;5:3u");
    tui.render_now(false);
    assert_eq!(transcript.borrow().scroll_top(), 0);

    tui.handle_terminal_input("\x1b[6~");
    tui.render_now(false);
    assert_eq!(transcript.borrow().scroll_top(), 1);
    assert_eq!(
        *inputs.borrow(),
        editor_keys
            .iter()
            .map(|key| key.to_string())
            .collect::<Vec<_>>()
    );
    stop(&mut tui);
}

/// 21 `jumps between OSC 133 semantic prompt markers`
#[test]
fn jumps_between_osc_133_semantic_prompt_markers() {
    let (mut tui, _) = alt(20, 3);
    let osc133 = "\x1b]133;A\x07";
    tui.add_child(Box::new(text(
        (1..=4)
            .flat_map(|message| [format!("{osc133}message {message}"), "detail".to_string()])
            .collect::<Vec<_>>()
            .join("\n"),
    )));
    tui.start();
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 5);

    tui.handle_terminal_input("\x1b[57419;6u");
    tui.handle_terminal_input("\x1b[57419;6:3u");
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 4);
    assert_eq!(screen(&tui)[0], "message 3");

    tui.handle_terminal_input("\x1b[1;6A");
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 2);
    assert_eq!(screen(&tui)[0], "message 2");

    tui.handle_terminal_input("\x1b[57420;6u");
    tui.handle_terminal_input("\x1b[57420;6:3u");
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 4);
    assert_eq!(screen(&tui)[0], "message 3");

    tui.handle_terminal_input("\x1b[1;6B");
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 5);
    assert_eq!(screen(&tui)[1], "message 4");
    assert!(tui.is_following_output());
    stop(&mut tui);
}

// ---------------------------------------------------------------------------
// 搜索（22-29）
// ---------------------------------------------------------------------------

fn utf16_find(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .find(needle)
        .map(|byte| haystack[..byte].encode_utf16().count())
}

fn utf16_rfind(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .rfind(needle)
        .map(|byte| haystack[..byte].encode_utf16().count())
}

/// 22 `searches normalized rendered transcript text across rows`
#[test]
fn searches_normalized_rendered_transcript_text_across_rows() {
    use kiana_tui::alt_screen_search::{
        AltScreenSearchMatch, AltScreenSearchSegment, find_alt_screen_search_matches,
    };
    assert_eq!(
        find_alt_screen_search_matches(
            &["alpha QUICK".to_string(), "brown fox".to_string()],
            "quick brown"
        ),
        vec![AltScreenSearchMatch {
            segments: vec![
                AltScreenSearchSegment {
                    row: 0,
                    start_col: 6,
                    end_col: 11,
                },
                AltScreenSearchSegment {
                    row: 1,
                    start_col: 0,
                    end_col: 5,
                },
            ],
        }]
    );
}

/// 23 `maps normalized ASCII and Unicode search matches back to rendered columns`
#[test]
fn maps_normalized_ascii_and_unicode_search_matches_back_to_rendered_columns() {
    use kiana_tui::alt_screen_search::{
        AltScreenSearchMatch, AltScreenSearchSegment, find_alt_screen_search_matches,
    };
    assert_eq!(
        find_alt_screen_search_matches(
            &[
                "\x1b[31mfoo  bar\x1b[0m".to_string(),
                "A界🙂e\u{301}Z".to_string(),
            ],
            "oo   bar\nA界🙂e\u{301}"
        ),
        vec![AltScreenSearchMatch {
            segments: vec![
                AltScreenSearchSegment {
                    row: 0,
                    start_col: 1,
                    end_col: 3,
                },
                AltScreenSearchSegment {
                    row: 0,
                    start_col: 5,
                    end_col: 8,
                },
                AltScreenSearchSegment {
                    row: 1,
                    start_col: 0,
                    end_col: 6,
                },
            ],
        }]
    );
}

/// 24 `reuses indexed transcript matches until the query or rendered lines change`
#[test]
fn reuses_indexed_transcript_matches_until_the_query_or_rendered_lines_change() {
    use kiana_tui::alt_screen_search::{AltScreenSearchIndex, AltScreenSearchSegment};
    let mut index = AltScreenSearchIndex::new();
    let initial = index.search(&["alpha needle".to_string(), "omega".to_string()], "needle");
    assert!(initial.changed);
    assert_eq!(initial.matches.len(), 1);

    let cached = index.search(&["alpha needle".to_string(), "omega".to_string()], "needle");
    assert!(!cached.changed);
    assert!(Rc::ptr_eq(&cached.matches, &initial.matches));

    let changed_query = index.search(&["alpha needle".to_string(), "omega".to_string()], "omega");
    assert!(changed_query.changed);
    assert!(!Rc::ptr_eq(&changed_query.matches, &initial.matches));
    assert_eq!(
        changed_query.matches[0].segments,
        vec![AltScreenSearchSegment {
            row: 1,
            start_col: 0,
            end_col: 5
        }]
    );

    let changed_lines = index.search(
        &["alpha needle".to_string(), "no match".to_string()],
        "omega",
    );
    assert!(changed_lines.changed);
    assert!(changed_lines.matches.is_empty());
}

/// 25 `renders transcript search with a muted placeholder and right-aligned controls`
#[test]
fn renders_transcript_search_with_a_muted_placeholder_and_right_aligned_controls() {
    use kiana_tui::alt_screen_search::AltScreenSearchComponent;
    let mut component = AltScreenSearchComponent::new(Box::new(|_query| {}), None);
    let rendered = component.render(48);
    let lines: Vec<String> = rendered
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect();

    assert_eq!(lines.len(), 3);
    assert!(lines.iter().all(|line| visible_width(line) == 48));
    // TS：`/^┌─+┐$/`、`/^│ Find in transcript +│$/`、`/^└─+ ↑ Shift\+Enter · ↓ Enter ─┘$/`
    // （逐字布局：左右 rule 宽度、占位符后的尾部 padding、右对齐控制条）。
    assert_eq!(lines[0], format!("┌{}┐", "─".repeat(46)));
    assert_eq!(lines[1], format!("│ Find in transcript{}│", " ".repeat(27)));
    assert_eq!(
        lines[2],
        format!("└{} ↑ Shift+Enter · ↓ Enter ─┘", "─".repeat(20))
    );
    assert!(rendered[1].contains("\x1b[2m"));
    let controls = &lines[2];
    assert_eq!(
        component.get_navigation_direction_at(2, utf16_find(controls, "↑").unwrap()),
        Some(-1)
    );
    assert_eq!(
        component.get_navigation_direction_at(2, utf16_find(controls, "Shift+Enter").unwrap() + 5),
        Some(-1)
    );
    assert_eq!(
        component.get_navigation_direction_at(2, utf16_find(controls, "·").unwrap()),
        None
    );
    assert_eq!(
        component.get_navigation_direction_at(2, utf16_find(controls, "↓").unwrap()),
        Some(1)
    );
    assert_eq!(
        component.get_navigation_direction_at(2, utf16_rfind(controls, "Enter").unwrap() + 2),
        Some(1)
    );

    component.handle_input("n");
    component.set_result(0, 2);
    let populated_render = component.render(48);
    let populated: Vec<String> = populated_render
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect();
    assert!(populated[1].contains('n'));
    assert!(populated[1].contains("1/2"));
    assert!(populated_render[1].contains("\x1b[2m 1/2 \x1b[22m"));
    assert!(
        !populated
            .iter()
            .any(|line| line.contains("Find in transcript"))
    );
}

/// 26 `navigates transcript search with hoverable arrow buttons and toggles it with its shortcut`
#[test]
fn navigates_transcript_search_with_hoverable_arrow_buttons_and_toggles_it_with_its_shortcut() {
    let (mut tui, output) = alt_with(
        120,
        6,
        TuiAltScreenOptions {
            search_navigation_button_style: Some(std::rc::Rc::new(|text: &str, hovered: bool| {
                format!(
                    "{}{text}\x1b[49m",
                    if hovered { "\x1b[45m" } else { "\x1b[44m" }
                )
            })),
            ..Default::default()
        },
    );
    tui.add_child(Box::new(text("needle one\nmiddle\nneedle two\nend")));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[102;6u");
    tui.handle_terminal_input("needle");
    tui.render_now(false);
    let viewport = padded_screen(&tui);
    assert!(viewport.iter().any(|line| line.contains("1/2")));
    assert!(
        viewport
            .iter()
            .any(|line| line.contains("↑ Shift+Enter · ↓ Enter"))
    );

    let arrow_row = viewport
        .iter()
        .position(|line| line.contains('↑') && line.contains('↓'))
        .expect("arrow row");
    let arrow_column = utf16_rfind(&viewport[arrow_row], "Enter").expect("arrow column");
    let before_hover = output.borrow().len();
    tui.handle_terminal_input(&format!("\x1b[<35;{};{}M", arrow_column + 1, arrow_row + 1));
    tui.render_now(false);
    let hovered_writes = output.borrow()[before_hover..].to_string();
    assert!(
        hovered_writes.contains("\x1b[45m↓ Enter\x1b[49m"),
        "hovered writes: {hovered_writes:?}"
    );

    tui.handle_terminal_input(&format!("\x1b[<0;{};{}M", arrow_column + 1, arrow_row + 1));
    tui.render_now(false);
    let viewport = padded_screen(&tui);
    assert!(viewport.iter().any(|line| line.contains("2/2")));
    assert!(
        viewport
            .iter()
            .any(|line| line.contains("↑ Shift+Enter · ↓ Enter"))
    );

    let arrow_row = viewport
        .iter()
        .position(|line| line.contains('↑') && line.contains('↓'))
        .expect("arrow row");
    let arrow_column = utf16_find(&viewport[arrow_row], "Shift+Enter").expect("previous") + 3;
    tui.handle_terminal_input(&format!("\x1b[<0;{};{}M", arrow_column + 1, arrow_row + 1));
    tui.render_now(false);
    assert!(padded_screen(&tui).iter().any(|line| line.contains("1/2")));
    assert!(
        padded_screen(&tui)
            .iter()
            .any(|line| line.contains("↑ Shift+Enter · ↓ Enter"))
    );

    tui.handle_terminal_input("\x1b[102;6u");
    tui.render_now(false);
    assert!(
        !padded_screen(&tui)
            .iter()
            .any(|line| line.contains("↑ Shift+Enter · ↓ Enter"))
    );
    stop(&mut tui);
}

/// 27 `does not treat transcript box drawing as search navigation buttons`
#[test]
fn does_not_treat_transcript_box_drawing_as_search_navigation_buttons() {
    let (mut tui, _) = alt(80, 10);
    tui.add_child(Box::new(text(
        [
            "needle one",
            "middle",
            "needle two",
            "filler",
            "┌────────────────────────────────────────┐",
            "│ box                                    │",
            "└────────────────────────────────────────┘",
            "end",
        ]
        .join("\n"),
    )));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[102;6u");
    tui.handle_terminal_input("needle");
    tui.render_now(false);
    let viewport = padded_screen(&tui);
    assert!(viewport.iter().any(|line| line.contains("1/2")));
    assert!(!viewport.iter().any(|line| line.contains("2/2")));

    let box_bottom_row = viewport
        .iter()
        .position(|line| line.starts_with('└'))
        .expect("box bottom row");
    tui.handle_terminal_input(&format!("\x1b[<0;24;{}M", box_bottom_row + 1));
    tui.render_now(false);

    let viewport = padded_screen(&tui);
    assert!(viewport.iter().any(|line| line.contains("1/2")));
    assert!(!viewport.iter().any(|line| line.contains("2/2")));
    stop(&mut tui);
}

/// 28 `uses configured styles for current and non-current search matches`
#[test]
fn uses_configured_styles_for_current_and_non_current_search_matches() {
    let (mut tui, output) = alt_with(
        60,
        4,
        TuiAltScreenOptions {
            search_match_style: Some(std::rc::Rc::new(|text: &str| {
                format!("\x1b[41m{text}\x1b[49m")
            })),
            search_current_match_style: Some(std::rc::Rc::new(|text: &str| {
                format!("\x1b[42m{text}\x1b[49m")
            })),
            ..Default::default()
        },
    );
    tui.add_child(Box::new(text("needle first\nmiddle\nneedle second\nend")));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[102;6u");
    tui.handle_terminal_input("needle");
    tui.render_now(false);

    let writes = output.borrow().clone();
    assert!(writes.contains("\x1b[42mneedle\x1b[49m"));
    assert!(writes.contains("\x1b[41mneedle\x1b[49m"));
    stop(&mut tui);
}

/// 29 `searches the transcript with Ctrl+Shift+F and restores editor focus on close`
#[test]
fn searches_the_transcript_with_ctrl_shift_f_and_restores_editor_focus_on_close() {
    let (mut tui, output) = alt(60, 8);
    let transcript_text = shared(text(
        (0..12)
            .map(|index| {
                if index == 4 {
                    "line 5 needle one".to_string()
                } else if index == 9 {
                    "line 10 needle two".to_string()
                } else {
                    format!("line {}", index + 1)
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
    ));
    let transcript = shared(
        ScrollView::new(
            Box::new(SharedRef(as_shared(&transcript_text))),
            ScrollViewOptions {
                follow: Some(ScrollViewFollow::End),
                primary: Some(true),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let inputs: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let editor = shared(RecordingEditor {
        inputs: Rc::clone(&inputs),
    });
    let root = VStack::new(
        vec![
            stack_entry(
                as_shared(&transcript),
                StackEntryOptions {
                    basis: Some(0.0),
                    grow: Some(1.0),
                    min_size: Some(1.0),
                    ..Default::default()
                },
            ),
            stack_entry(
                as_shared(&editor),
                StackEntryOptions {
                    basis: Some(1.0),
                    shrink: Some(0.0),
                    ..Default::default()
                },
            ),
        ],
        StackOptions::default(),
    );
    tui.set_layout_root_component(Some(Box::new(root)));
    focus_shared(&mut tui, &as_shared(&editor));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[102;6u");
    tui.handle_terminal_input("needle");
    tui.render_now(false);
    assert!(!transcript.borrow().is_following_end());
    let viewport = padded_screen(&tui);
    assert!(viewport.iter().any(|line| line.contains("2/2")));
    assert!(
        viewport
            .iter()
            .any(|line| line.contains("↑ Shift+Enter · ↓ Enter"))
    );
    assert!(
        viewport
            .iter()
            .any(|line| line.contains("line 10 needle two"))
    );
    assert!(inputs.borrow().is_empty());
    assert!(output.borrow().contains("\x1b[1;7mneedle\x1b[22;27m"));

    for _ in 0..6 {
        tui.handle_terminal_input("\x1b[<64;1;4M");
    }
    tui.render_now(false);
    assert_eq!(transcript.borrow().scroll_top(), 0);
    assert!(
        padded_screen(&tui)
            .iter()
            .any(|line| line.contains("needle") && line.contains("2/2"))
    );

    tui.handle_terminal_input("\x07");
    tui.render_now(false);
    assert!(padded_screen(&tui).iter().any(|line| line.contains("1/2")));
    assert!(
        padded_screen(&tui)
            .iter()
            .any(|line| line.contains("line 5 needle one"))
    );

    tui.handle_terminal_input("\x1b[103;6u");
    tui.render_now(false);
    assert!(padded_screen(&tui).iter().any(|line| line.contains("2/2")));
    assert!(
        padded_screen(&tui)
            .iter()
            .any(|line| line.contains("line 10 needle two"))
    );

    tui.handle_terminal_input("\x1b");
    tui.handle_terminal_input("x");
    tui.render_now(false);
    assert!(
        !padded_screen(&tui)
            .iter()
            .any(|line| line.contains("↑ Shift+Enter · ↓ Enter"))
    );
    assert_eq!(*inputs.borrow(), vec!["x".to_string()]);
    stop(&mut tui);
}

// ---------------------------------------------------------------------------
// 图片（30-39）
// ---------------------------------------------------------------------------

fn set_images(protocol: kiana_tui::terminal_image::ImageProtocol) {
    kiana_tui::terminal_image::set_capabilities(kiana_tui::terminal_image::TerminalCapabilities {
        images: Some(protocol),
        true_color: true,
        hyperlinks: true,
    });
}

fn kitty_line(base64: &str, rows: f64, image_id: u64) -> String {
    kiana_tui::terminal_image::encode_kitty(
        base64,
        kiana_tui::terminal_image::KittyEncodeOptions {
            columns: Some(2.0),
            rows: Some(rows),
            image_id: Some(image_id as f64),
            move_cursor: Some(false),
        },
    )
}

/// 30 `does not emit Kitty graphics commands or OSC 133 zones in iTerm2`
#[test]
fn does_not_emit_kitty_graphics_commands_or_osc_133_zones_in_iterm2() {
    set_images(kiana_tui::terminal_image::ImageProtocol::Iterm2);
    let (mut tui, output) = alt(20, 3);
    struct Osc133Content;
    impl Component for Osc133Content {
        fn render(&mut self, _width: usize) -> Vec<String> {
            vec!["\x1b]133;B\x07\x1b]133;C\x07\x1b]133;A\x07content".to_string()]
        }
    }
    tui.add_child(Box::new(Osc133Content));
    tui.add_child(Box::new(kiana_tui::components::image::Image::new(
        "AAAA",
        "image/png",
        kiana_tui::components::image::ImageTheme::plain(),
        kiana_tui::components::image::ImageOptions {
            filename: Some("example.png".to_string()),
            ..Default::default()
        },
        Some(kiana_tui::terminal_image::ImageDimensions {
            width_px: 10.0,
            height_px: 10.0,
        }),
    )));
    tui.start();
    tui.render_now(false);
    stop(&mut tui);
    let writes = output.borrow().clone();
    assert!(!writes.contains("\x1b_G"));
    assert!(!writes.contains("\x1b]133;"));
    assert!(!writes.contains("\x1b]1337;File="));
    assert!(writes.contains("[Image:"));
    kiana_tui::terminal_image::reset_capabilities_cache();
}

/// 31 `clears stale iTerm2 image placements when they leave the viewport`
#[test]
fn clears_stale_iterm2_image_placements_when_they_leave_the_viewport() {
    set_images(kiana_tui::terminal_image::ImageProtocol::Iterm2);
    let (mut tui, output) = alt(20, 3);
    struct ImageContent;
    impl Component for ImageContent {
        fn render(&mut self, _width: usize) -> Vec<String> {
            vec![
                "\x1b]1337;File=inline=1;width=2;height=auto:AAAA\x07".to_string(),
                String::new(),
                String::new(),
                "after".to_string(),
                "more".to_string(),
                "end".to_string(),
            ]
        }
    }
    tui.add_child(Box::new(ImageContent));
    tui.start();
    tui.render_now(false);
    tui.scroll_to_top();
    tui.render_now(false);
    let before = output.borrow().len();

    tui.scroll_by(1);
    tui.render_now(false);
    let writes = output.borrow()[before..].to_string();
    assert!(writes.contains("\x1b[2J"), "writes: {writes:?}");
    stop(&mut tui);
    kiana_tui::terminal_image::reset_capabilities_cache();
}

/// 32 `crops a Kitty image whose first line is above the viewport`
#[test]
fn crops_a_kitty_image_whose_first_line_is_above_the_viewport() {
    set_images(kiana_tui::terminal_image::ImageProtocol::Kitty);
    let (mut tui, output) = alt(20, 3);
    let image_id = 123u64;
    let image_line = kitty_line("AAAA", 3.0, image_id);
    kiana_tui::terminal_image::register_kitty_image_metadata(
        kiana_tui::terminal_image::KittyImageMetadata {
            image_id,
            columns: 2,
            rows: 3,
            width_px: 100.0,
            height_px: 100.0,
        },
    );
    struct ImageContent(String);
    impl Component for ImageContent {
        fn render(&mut self, _width: usize) -> Vec<String> {
            vec![
                "before".to_string(),
                self.0.clone(),
                String::new(),
                String::new(),
                "after".to_string(),
                "end".to_string(),
            ]
        }
    }
    tui.add_child(Box::new(ImageContent(image_line)));
    tui.start();
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 3);
    let writes = output.borrow().clone();
    assert!(
        writes.contains("i=123") && writes.contains("y=66,h=34,r=1"),
        "writes: {writes:?}"
    );
    // `queryCellSize()`：能力里有图片时向终端查询单元格像素尺寸（CSI 16 t）。
    assert!(writes.contains("\x1b[16t"), "writes: {writes:?}");
    stop(&mut tui);
    kiana_tui::terminal_image::reset_capabilities_cache();
}

/// 33 `crops a Kitty image in a padded, colored box whose first line is above the viewport`
#[test]
fn crops_a_kitty_image_in_a_padded_colored_box_whose_first_line_is_above_the_viewport() {
    set_images(kiana_tui::terminal_image::ImageProtocol::Kitty);
    let (mut tui, output) = alt(20, 3);
    let image_id = 11271u64;
    let image_line = kitty_line("AAAA", 3.0, image_id);
    kiana_tui::terminal_image::register_kitty_image_metadata(
        kiana_tui::terminal_image::KittyImageMetadata {
            image_id,
            columns: 2,
            rows: 3,
            width_px: 100.0,
            height_px: 100.0,
        },
    );
    struct ImageContent(String);
    impl Component for ImageContent {
        fn render(&mut self, _width: usize) -> Vec<String> {
            vec![self.0.clone(), String::new(), String::new()]
        }
    }
    let mut boxed = BoxComponent::new(
        1,
        0,
        Some(std::sync::Arc::new(|text: &str| {
            format!("\x1b[44m{text}\x1b[49m")
        })),
    );
    boxed.add_child(Box::new(ImageContent(image_line)));
    struct Static(Vec<String>);
    impl Component for Static {
        fn render(&mut self, _width: usize) -> Vec<String> {
            self.0.clone()
        }
    }
    tui.add_child(Box::new(Static(vec!["before".to_string()])));
    tui.add_child(Box::new(boxed));
    tui.add_child(Box::new(Static(vec![
        "after".to_string(),
        "end".to_string(),
    ])));
    tui.start();
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 3);
    let writes = output.borrow().clone();
    assert!(
        writes.contains("i=11271") && writes.contains("y=66,h=34,r=1"),
        "writes: {writes:?}"
    );
    stop(&mut tui);
    kiana_tui::terminal_image::reset_capabilities_cache();
}

/// 34 `crops a Kitty image whose placeholder rows contain text when its first line is above the viewport`
#[test]
fn crops_a_kitty_image_whose_placeholder_rows_contain_text_when_its_first_line_is_above_the_viewport()
 {
    set_images(kiana_tui::terminal_image::ImageProtocol::Kitty);
    let (mut tui, output) = alt(20, 3);
    let image_id = 11272u64;
    let image_line = kitty_line("AAAA", 3.0, image_id);
    kiana_tui::terminal_image::register_kitty_image_metadata(
        kiana_tui::terminal_image::KittyImageMetadata {
            image_id,
            columns: 2,
            rows: 3,
            width_px: 100.0,
            height_px: 100.0,
        },
    );
    struct ImageContent(String);
    impl Component for ImageContent {
        fn render(&mut self, _width: usize) -> Vec<String> {
            vec![
                "before".to_string(),
                self.0.clone(),
                "   | side".to_string(),
                "   | side".to_string(),
            ]
        }
    }
    struct Static(Vec<String>);
    impl Component for Static {
        fn render(&mut self, _width: usize) -> Vec<String> {
            self.0.clone()
        }
    }
    tui.add_child(Box::new(ImageContent(image_line)));
    tui.add_child(Box::new(Static(vec![
        "after".to_string(),
        "end".to_string(),
    ])));
    tui.start();
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 3);
    let writes = output.borrow().clone();
    assert!(
        writes.contains("i=11272") && writes.contains("y=66,h=34,r=1"),
        "writes: {writes:?}"
    );
    stop(&mut tui);
    kiana_tui::terminal_image::reset_capabilities_cache();
}

/// 可变的静态内容组件（测试用）。
struct DynContent(Rc<RefCell<Vec<String>>>);

impl Component for DynContent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.0.borrow().clone()
    }
}

/// 35 `redraws WezTerm Kitty images after writes to covered rows`
#[test]
fn redraws_wezterm_kitty_images_after_writes_to_covered_rows() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let saved = env_snapshot(&["WEZTERM_PANE"]);
    unsafe { std::env::set_var("WEZTERM_PANE", "1") };
    set_images(kiana_tui::terminal_image::ImageProtocol::Kitty);
    let (mut tui, output) = alt(20, 4);
    let image_id = 10319u64;
    let image_line = kitty_line("AAAA", 3.0, image_id);
    kiana_tui::terminal_image::register_kitty_image_metadata(
        kiana_tui::terminal_image::KittyImageMetadata {
            image_id,
            columns: 2,
            rows: 3,
            width_px: 100.0,
            height_px: 100.0,
        },
    );
    let lines = Rc::new(RefCell::new(vec![
        image_line,
        String::new(),
        String::new(),
        "after".to_string(),
    ]));
    tui.set_layout_root_component(Some(Box::new(DynContent(Rc::clone(&lines)))));
    tui.start();
    tui.render_now(false);
    let before = output.borrow().len();

    lines.borrow_mut()[1] = "changed".to_string();
    tui.request_render(false);
    tui.render_now(false);
    let writes = output.borrow()[before..].to_string();
    let placement_index = writes.find("\x1b_Ga=p,q=2");
    assert!(
        writes.contains("\x1b_Ga=d,d=a,q=2\x1b\\"),
        "writes: {writes:?}"
    );
    assert!(
        placement_index.unwrap_or(0) > writes.find("changed").unwrap_or(0),
        "writes: {writes:?}"
    );
    assert!(!writes.contains("\x1b_Ga=T"), "writes: {writes:?}");
    stop(&mut tui);
    kiana_tui::terminal_image::reset_capabilities_cache();
    env_restore(saved);
}

/// 36 `reuses moved Kitty images without dropping HStack siblings`
#[test]
fn reuses_moved_kitty_images_without_dropping_hstack_siblings() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    set_images(kiana_tui::terminal_image::ImageProtocol::Kitty);
    let (mut tui, output) = alt(20, 6);
    let label = shared(text("left"));
    let header = shared(text("header"));
    let image = shared(kiana_tui::components::image::Image::new(
        "A".repeat(8192),
        "image/png",
        kiana_tui::components::image::ImageTheme::plain(),
        Default::default(),
        Some(kiana_tui::terminal_image::ImageDimensions {
            width_px: 100.0,
            height_px: 100.0,
        }),
    ));
    let row = HStack::new(
        vec![
            stack_entry(
                as_shared(&label),
                StackEntryOptions {
                    basis: Some(10.0),
                    ..Default::default()
                },
            ),
            stack_entry(
                as_shared(&image),
                StackEntryOptions {
                    basis: Some(10.0),
                    ..Default::default()
                },
            ),
        ],
        StackOptions::default(),
    );
    let root = VStack::new(
        vec![
            stack_entry(
                as_shared(&header),
                StackEntryOptions {
                    basis: None,
                    ..Default::default()
                },
            ),
            StackChild::Entry(StackEntry::with_options(
                Box::new(row),
                StackEntryOptions {
                    basis: Some(4.0),
                    ..Default::default()
                },
            )),
        ],
        StackOptions::default(),
    );
    tui.set_layout_root_component(Some(Box::new(root)));
    tui.start();
    tui.render_now(false);
    assert!(output.borrow().contains("\x1b_Ga=T"));

    let before = output.borrow().len();
    label.borrow_mut().set_text("changed");
    header.borrow_mut().set_text("header\nsecond");
    tui.request_render(false);
    tui.render_now(false);
    let writes = output.borrow()[before..].to_string();
    let placement_index = writes.find("\x1b_Ga=p,q=2");
    assert!(
        writes.contains("\x1b_Ga=d,d=a,q=2\x1b\\"),
        "writes: {writes:?}"
    );
    assert!(
        placement_index.unwrap_or(0) > writes.find("changed").unwrap_or(0),
        "writes: {writes:?}"
    );
    assert!(!writes.contains("\x1b_Ga=T"), "writes: {writes:?}");
    assert!(
        writes.len() < 2000,
        "expected placement-only redraw, got {} bytes",
        writes.len()
    );
    assert!(screen(&tui).iter().any(|line| line.trim_end() == "changed"));
    stop(&mut tui);
    kiana_tui::terminal_image::reset_capabilities_cache();
}

/// 37 `retains recently offscreen Kitty images for placement-only reuse`
#[test]
fn retains_recently_offscreen_kitty_images_for_placement_only_reuse() {
    set_images(kiana_tui::terminal_image::ImageProtocol::Kitty);
    let (mut tui, output) = alt(20, 1);
    let image_id = 321u64;
    let image_line = kitty_line("AAAA", 1.0, image_id);
    kiana_tui::terminal_image::register_kitty_image_metadata(
        kiana_tui::terminal_image::KittyImageMetadata {
            image_id,
            columns: 2,
            rows: 1,
            width_px: 100.0,
            height_px: 50.0,
        },
    );
    let lines = Rc::new(RefCell::new(vec![image_line, "after".to_string()]));
    let scroll_view = shared(
        ScrollView::new(
            Box::new(DynContent(Rc::clone(&lines))),
            ScrollViewOptions {
                primary: Some(true),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    tui.set_layout_root_component(Some(Box::new(SharedRef(as_shared(&scroll_view)))));
    tui.start();
    tui.render_now(false);
    assert!(output.borrow().contains("\x1b_Ga=T"));

    let before = output.borrow().len();
    tui.scroll_by(1);
    tui.render_now(false);
    tui.scroll_by(-1);
    tui.render_now(false);
    let writes = output.borrow()[before..].to_string();
    assert!(writes.contains("\x1b_Ga=p,q=2"), "writes: {writes:?}");
    assert!(!writes.contains("\x1b_Ga=T"), "writes: {writes:?}");
    assert!(
        !writes.contains(&format!("\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\")),
        "writes: {writes:?}"
    );
    stop(&mut tui);
    kiana_tui::terminal_image::reset_capabilities_cache();
}

/// 38 `evicts the least recently visible Kitty image when the cache is full`
#[test]
fn evicts_the_least_recently_visible_kitty_image_when_the_cache_is_full() {
    set_images(kiana_tui::terminal_image::ImageProtocol::Kitty);
    let (mut tui, output) = alt(20, 1);
    let first_image_id = 500u64;
    let mut image_lines = Vec::new();
    for index in 0..18u64 {
        let image_id = first_image_id + index;
        kiana_tui::terminal_image::register_kitty_image_metadata(
            kiana_tui::terminal_image::KittyImageMetadata {
                image_id,
                columns: 2,
                rows: 1,
                width_px: 100.0,
                height_px: 50.0,
            },
        );
        image_lines.push(kitty_line("AAAA", 1.0, image_id));
    }
    let lines = Rc::new(RefCell::new(image_lines.clone()));
    let scroll_view = shared(
        ScrollView::new(
            Box::new(DynContent(Rc::clone(&lines))),
            ScrollViewOptions {
                primary: Some(true),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    tui.set_layout_root_component(Some(Box::new(SharedRef(as_shared(&scroll_view)))));
    tui.start();
    tui.render_now(false);
    for _ in 1..image_lines.len() {
        tui.scroll_by(1);
        tui.render_now(false);
    }
    assert!(
        output
            .borrow()
            .contains(&format!("\x1b_Ga=d,d=I,i={first_image_id},q=2\x1b\\")),
        "expected eviction of the first image"
    );

    let before = output.borrow().len();
    tui.scroll_to_top();
    tui.render_now(false);
    let writes = output.borrow()[before..].to_string();
    assert!(writes.contains("\x1b_Ga=T"), "writes: {writes:?}");
    stop(&mut tui);
    kiana_tui::terminal_image::reset_capabilities_cache();
}

/// 39 `evicts offscreen Kitty images when decoded raster memory exceeds the cache quota`
#[test]
fn evicts_offscreen_kitty_images_when_decoded_raster_memory_exceeds_the_cache_quota() {
    set_images(kiana_tui::terminal_image::ImageProtocol::Kitty);
    let (mut tui, output) = alt(20, 1);
    let first_image_id = 600u64;
    let mut image_lines = Vec::new();
    for index in 0..4u64 {
        let image_id = first_image_id + index;
        kiana_tui::terminal_image::register_kitty_image_metadata(
            kiana_tui::terminal_image::KittyImageMetadata {
                image_id,
                columns: 2,
                rows: 1,
                width_px: 3840.0,
                height_px: 2160.0,
            },
        );
        image_lines.push(kitty_line("AAAA", 1.0, image_id));
    }
    let lines = Rc::new(RefCell::new(image_lines.clone()));
    let scroll_view = shared(
        ScrollView::new(
            Box::new(DynContent(Rc::clone(&lines))),
            ScrollViewOptions {
                primary: Some(true),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    tui.set_layout_root_component(Some(Box::new(SharedRef(as_shared(&scroll_view)))));
    tui.start();
    tui.render_now(false);
    for _ in 1..image_lines.len() {
        tui.scroll_by(1);
        tui.render_now(false);
    }
    assert!(
        output
            .borrow()
            .contains(&format!("\x1b_Ga=d,d=I,i={first_image_id},q=2\x1b\\")),
        "expected decoded-quota eviction of the first image"
    );
    stop(&mut tui);
    kiana_tui::terminal_image::reset_capabilities_cache();
}

// ---------------------------------------------------------------------------
// 超链接与选择（40-57）
// ---------------------------------------------------------------------------

fn osc52(text: &str) -> String {
    use base64::Engine as _;
    format!(
        "\x1b]52;c;{}\x07",
        base64::engine::general_purpose::STANDARD.encode(text.as_bytes())
    )
}

/// 40 `opens an OSC 8 hyperlink with specific or generic release codes, but not on drag`
#[test]
fn opens_an_osc8_hyperlink_with_specific_or_generic_release_codes_but_not_on_drag() {
    let opened: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&opened);
    let (mut tui, _) = alt_with(
        20,
        3,
        TuiAltScreenOptions {
            open_url: Some(std::rc::Rc::new(move |url: &str| {
                sink.borrow_mut().push(url.to_string());
            })),
            ..Default::default()
        },
    );
    let url = "https://example.com/path?q=1";
    let bel_url = "https://example.com/bel";
    let emoji_url = "https://example.com/emoji";
    tui.add_child(Box::new(text(format!(
        "{}\n\x1b]8;;{bel_url}\x07link\x1b]8;;\x07\n{}",
        kiana_tui::terminal_image::hyperlink("link", url),
        kiana_tui::terminal_image::hyperlink("🙂", emoji_url)
    ))));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;2;1M");
    tui.handle_terminal_input("\x1b[<3;2;1m");
    tui.render_now(false);
    assert_eq!(*opened.borrow(), vec![url.to_string()]);

    tui.handle_terminal_input("\x1b[<0;2;2M");
    tui.handle_terminal_input("\x1b[<0;2;2m");
    tui.render_now(false);
    assert_eq!(*opened.borrow(), vec![url.to_string(), bel_url.to_string()]);

    tui.handle_terminal_input("\x1b[<0;2;3M");
    tui.handle_terminal_input("\x1b[<0;2;3m");
    tui.render_now(false);
    assert_eq!(
        *opened.borrow(),
        vec![url.to_string(), bel_url.to_string(), emoji_url.to_string()]
    );

    tui.handle_terminal_input("\x1b[<0;2;1M");
    tui.handle_terminal_input("\x1b[<32;4;1M");
    tui.handle_terminal_input("\x1b[<0;4;1m");
    tui.render_now(false);
    assert_eq!(
        *opened.borrow(),
        vec![url.to_string(), bel_url.to_string(), emoji_url.to_string()]
    );
    stop(&mut tui);
}

/// 41 `selects visible text with the mouse and copies it with OSC 52 after a generic release`
#[test]
fn selects_visible_text_with_the_mouse_and_copies_it_with_osc_52_after_a_generic_release() {
    let (mut tui, output) = alt(20, 4);
    tui.add_child(Box::new(text("\x1b[1mal\x1b[0mpha\nbeta\ngamma\ndelta")));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;1;1M");
    tui.handle_terminal_input("\x1b[<32;4;2M");
    tui.handle_terminal_input("\x1b[<3;4;2m");
    tui.render_now(false);

    let writes = output.borrow().clone();
    assert!(
        writes.contains(&osc52("alpha\nbeta")),
        "writes missing osc52"
    );
    assert!(writes.contains("\x1b[7m"));
    assert!(
        writes.contains("al\x1b[0m\x1b[7mpha"),
        "selection inverse must be reapplied after a reset inside the selection"
    );
    assert!(
        padded_screen(&tui)
            .iter()
            .any(|line| line.contains("Copied!"))
    );
    stop(&mut tui);
}

/// 42 `keeps the selection highlight after a fake cursor cell`
#[test]
fn keeps_the_selection_highlight_after_a_fake_cursor_cell() {
    let (mut tui, output) = alt(20, 4);
    tui.add_child(Box::new(text(format!(
        "> {}bcd",
        kiana_tui::tui::render_fake_cursor("a")
    ))));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;1;1M");
    tui.handle_terminal_input("\x1b[<32;6;1M");
    tui.handle_terminal_input("\x1b[<3;6;1m");
    tui.render_now(false);

    assert!(
        output.borrow().contains("\x1b[27m\x1b[7mbcd"),
        "selection inverse must be reapplied after the fake cursor ends"
    );
    stop(&mut tui);
}

/// 43 `uses an injected copySelection handler instead of OSC 52 and reports success`
#[test]
fn uses_an_injected_copy_selection_handler_instead_of_osc_52_and_reports_success() {
    let copied: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&copied);
    let (mut tui, output) = alt_with(
        20,
        4,
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
    tui.add_child(Box::new(text("alpha\nbeta\ngamma\ndelta")));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;1;1M");
    tui.handle_terminal_input("\x1b[<32;4;2M");
    tui.handle_terminal_input("\x1b[<0;4;2m");
    tui.render_now(false);

    assert_eq!(*copied.borrow(), vec!["alpha\nbeta".to_string()]);
    assert!(!output.borrow().contains("\x1b]52;c;"));
    assert!(
        padded_screen(&tui)
            .iter()
            .any(|line| line.contains("Copied!"))
    );
    stop(&mut tui);
}

/// 44 `leaves selections visible without copying when copyOnSelect is disabled`
#[test]
fn leaves_selections_visible_without_copying_when_copy_on_select_is_disabled() {
    let copied: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&copied);
    let (mut tui, output) = alt_with(
        20,
        4,
        TuiAltScreenOptions {
            copy_on_select: Some(false),
            copy_selection: Some(std::rc::Rc::new(
                move |text: &str, _done: CopySelectionDone| {
                    sink.borrow_mut().push(text.to_string());
                    CopySelectionDispatch::Ready(CopySelectionResult::Ok)
                },
            )),
            ..Default::default()
        },
    );
    tui.add_child(Box::new(text("alpha\nbeta\ngamma\ndelta")));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;1;1M");
    tui.handle_terminal_input("\x1b[<32;4;2M");
    tui.handle_terminal_input("\x1b[<0;4;2m");
    tui.render_now(false);

    assert!(copied.borrow().is_empty());
    assert!(tui.has_active_selection());
    assert!(output.borrow().contains("\x1b[7m"));
    assert!(
        !padded_screen(&tui)
            .iter()
            .any(|line| line.contains("Copied!"))
    );
    stop(&mut tui);
}

/// 45 `copies an active selection programmatically`
#[test]
fn copies_an_active_selection_programmatically() {
    let copied: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&copied);
    let (mut tui, _) = alt_with(
        20,
        4,
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
    tui.add_child(Box::new(text("alpha\nbeta\ngamma\ndelta")));
    tui.start();
    tui.render_now(false);

    assert!(!tui.has_active_selection());
    assert_eq!(
        tui.copy_active_selection_to_clipboard(),
        CopySelectionStatus::Complete(false)
    );

    tui.handle_terminal_input("\x1b[<0;1;1M");
    tui.handle_terminal_input("\x1b[<32;4;2M");
    tui.handle_terminal_input("\x1b[<0;4;2m");
    tui.render_now(false);

    assert_eq!(*copied.borrow(), vec!["alpha\nbeta".to_string()]);
    assert!(tui.has_active_selection());

    copied.borrow_mut().clear();
    assert_eq!(
        tui.copy_active_selection_to_clipboard(),
        CopySelectionStatus::Complete(true)
    );
    tui.render_now(false);

    assert_eq!(*copied.borrow(), vec!["alpha\nbeta".to_string()]);
    assert!(
        padded_screen(&tui)
            .iter()
            .any(|line| line.contains("Copied!"))
    );
    stop(&mut tui);
}

/// `copySelection` 在 TS 里返回 `Promise<boolean | string>`，`copyTextToClipboard` 会
/// `await` 它再 flash（`tui-alt-screen.ts:1481-1516`）。完成回调模型下：宿主可稍后交付结果，
/// 完成前不 flash、不阻塞；完成时 flash 并请求一帧。
#[test]
fn defers_copy_flash_until_the_async_completion_arrives() {
    let done_slot: Rc<RefCell<Option<CopySelectionDone>>> = Rc::new(RefCell::new(None));
    let slot = Rc::clone(&done_slot);
    let (mut tui, _) = alt_with(
        20,
        4,
        TuiAltScreenOptions {
            copy_on_select: Some(false),
            copy_selection: Some(std::rc::Rc::new(
                move |_text: &str, done: CopySelectionDone| {
                    *slot.borrow_mut() = Some(done);
                    CopySelectionDispatch::Pending
                },
            )),
            ..Default::default()
        },
    );
    tui.add_child(Box::new(text("alpha\nbeta\ngamma\ndelta")));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;1;1M");
    tui.handle_terminal_input("\x1b[<32;4;2M");
    tui.handle_terminal_input("\x1b[<0;4;2m");
    tui.render_now(false);

    let completed: Rc<RefCell<Option<bool>>> = Rc::new(RefCell::new(None));
    let completion_sink = Rc::clone(&completed);
    assert_eq!(
        tui.copy_active_selection_to_clipboard_with(Some(Box::new(move |ok| {
            *completion_sink.borrow_mut() = Some(ok);
        }))),
        CopySelectionStatus::Pending
    );
    tui.render_now(false);
    assert!(
        !padded_screen(&tui)
            .iter()
            .any(|line| line.contains("Copied!")),
        "flash must wait for the async completion"
    );
    assert!(matches!(
        tui.render_decision(),
        kiana_tui::tui::RenderDecision::Idle
    ));
    assert!(
        completed.borrow().is_none(),
        "await result must not resolve before the completion"
    );

    let done = done_slot.borrow_mut().take().expect("completion captured");
    done(CopySelectionResult::Ok);
    assert_eq!(*completed.borrow(), Some(true));
    // TS 的 `requestRender()` 也要经过 `nextTick` + 16ms 节流窗口。
    std::thread::sleep(Duration::from_millis(20));
    assert!(
        matches!(tui.render_decision(), kiana_tui::tui::RenderDecision::Now),
        "completion must request a frame"
    );
    tui.render_now(false);
    assert!(
        padded_screen(&tui)
            .iter()
            .any(|line| line.contains("Copied!"))
    );
    assert!(
        !padded_screen(&tui)
            .iter()
            .any(|line| line.contains("Copy failed"))
    );
    stop(&mut tui);
}

/// 46 `flashes an error when the injected copySelection handler fails`
#[test]
fn flashes_an_error_when_the_injected_copy_selection_handler_fails() {
    let (mut tui, output) = alt_with(
        20,
        4,
        TuiAltScreenOptions {
            copy_selection: Some(std::rc::Rc::new(|_text: &str, _done: CopySelectionDone| {
                CopySelectionDispatch::Ready(CopySelectionResult::Failed)
            })),
            ..Default::default()
        },
    );
    tui.add_child(Box::new(text("alpha\nbeta\ngamma\ndelta")));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;1;1M");
    tui.handle_terminal_input("\x1b[<32;4;2M");
    tui.handle_terminal_input("\x1b[<0;4;2m");
    tui.render_now(false);

    assert!(
        padded_screen(&tui)
            .iter()
            .any(|line| line.contains("Copy failed"))
    );
    assert!(!output.borrow().contains("\x1b]52;c;"));
    stop(&mut tui);
}

/// 47 `flashes a specific error returned by the injected copySelection handler`
#[test]
fn flashes_a_specific_error_returned_by_the_injected_copy_selection_handler() {
    let (mut tui, _) = alt_with(
        80,
        4,
        TuiAltScreenOptions {
            copy_on_select: Some(false),
            copy_selection: Some(std::rc::Rc::new(|_text: &str, _done: CopySelectionDone| {
                CopySelectionDispatch::Ready(CopySelectionResult::Error(
                    "Clipboard unavailable: install wl-clipboard".to_string(),
                ))
            })),
            ..Default::default()
        },
    );
    tui.add_child(Box::new(text("alpha\nbeta\ngamma\ndelta")));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;1;1M");
    tui.handle_terminal_input("\x1b[<32;4;2M");
    tui.handle_terminal_input("\x1b[<0;4;2m");
    tui.render_now(false);
    assert_eq!(
        tui.copy_active_selection_to_clipboard(),
        CopySelectionStatus::Complete(false)
    );
    tui.render_now(false);

    assert!(
        padded_screen(&tui)
            .iter()
            .any(|line| line.contains("Clipboard unavailable: install wl-clipboard"))
    );
    assert!(
        !padded_screen(&tui)
            .iter()
            .any(|line| line.contains("Copy failed"))
    );
    // TS 断言 `flashDuration === 5000`：错误 flash 用 `COPY_ERROR_FLASH_DURATION_MS`。
    // Rust 无法包装固有方法，改为读 flash 的到期时刻（宿主调度返回的剩余等待）。
    match tui.render_decision() {
        kiana_tui::tui::RenderDecision::Wait(ms) => {
            assert!(
                ms > 4_000.0 && ms <= 5_000.0,
                "error flash must last 5000ms, got {ms}"
            );
        }
        other => panic!("expected a pending 5000ms flash deadline, got {other:?}"),
    }
    stop(&mut tui);
}

/// 48 `does not append whitespace to double-click word highlighting`
#[test]
fn does_not_append_whitespace_to_double_click_word_highlighting() {
    let (mut tui, output) = alt(20, 1);
    tui.add_child(Box::new(text("foo  bar")));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;1;1M");
    tui.handle_terminal_input("\x1b[<0;1;1m");
    tui.handle_terminal_input("\x1b[<0;3;1M");
    tui.render_now(false);

    assert!(output.borrow().contains("foo\x1b[27m"));
    stop(&mut tui);
}

/// 49 `coalesces slash and hyphen separated segments for double-click word selection`
#[test]
fn coalesces_slash_and_hyphen_separated_segments_for_double_click_word_selection() {
    let cases = [
        ("extensions/starline/fixed-editor/compositor.ts", "starline"),
        ("earendil-works/kiana-tui", "works"),
        ("~/.kiana/agent/settings.json", "agent"),
        ("~\\Documents\\kiana", "Documents"),
        ("foo+bar", "bar"),
    ];
    for (line, needle) in cases {
        let copied: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&copied);
        let (mut tui, _) = alt_with(
            80,
            1,
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
        tui.add_child(Box::new(text(line)));
        tui.start();
        tui.render_now(false);

        let column = line.find(needle).expect("needle") + 1;
        tui.handle_terminal_input(&format!("\x1b[<0;{column};1M"));
        tui.handle_terminal_input(&format!("\x1b[<0;{column};1m"));
        tui.handle_terminal_input(&format!("\x1b[<0;{column};1M"));
        tui.handle_terminal_input(&format!("\x1b[<0;{column};1m"));
        tui.render_now(false);

        assert_eq!(*copied.borrow(), vec![line.to_string()], "line {line}");
        stop(&mut tui);
    }
}

/// 50 `highlights a complete whitespace segment during a word drag`
#[test]
fn highlights_a_complete_whitespace_segment_during_a_word_drag() {
    let (mut tui, output) = alt(20, 1);
    tui.add_child(Box::new(text("foo  bar")));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;1;1M");
    tui.handle_terminal_input("\x1b[<0;1;1m");
    tui.handle_terminal_input("\x1b[<0;2;1M");
    tui.handle_terminal_input("\x1b[<32;4;1M");
    tui.render_now(false);

    assert!(output.borrow().contains("foo  \x1b[27m"));
    stop(&mut tui);
}

/// 51 `selects whole words on double click, extends word drags, and selects lines on triple click`
#[test]
fn selects_whole_words_on_double_click_extends_word_drags_and_selects_lines_on_triple_click() {
    let (mut tui, output) = alt(20, 2);
    tui.add_child(Box::new(text("zero alpha beta\ngamma delta")));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;6;1M");
    tui.handle_terminal_input("\x1b[<0;6;1m");
    tui.handle_terminal_input("\x1b[<0;10;1M");
    tui.handle_terminal_input("\x1b[<0;10;1m");
    tui.render_now(false);
    assert!(output.borrow().contains(&osc52("alpha")));

    // 双击拖动包含经过的每个词，而不是半个词。
    tui.handle_terminal_input("\x1b[<0;12;1M");
    tui.handle_terminal_input("\x1b[<0;12;1m");
    tui.handle_terminal_input("\x1b[<0;14;1M");
    tui.handle_terminal_input("\x1b[<32;3;2M");
    tui.handle_terminal_input("\x1b[<0;3;2m");
    tui.render_now(false);
    assert!(output.borrow().contains(&osc52("beta\ngamma")));

    tui.handle_terminal_input("\x1b[<0;7;2M");
    tui.handle_terminal_input("\x1b[<0;7;2m");
    tui.handle_terminal_input("\x1b[<0;9;2M");
    tui.handle_terminal_input("\x1b[<0;9;2m");
    tui.handle_terminal_input("\x1b[<0;11;2M");
    tui.handle_terminal_input("\x1b[<0;11;2m");
    tui.render_now(false);
    assert!(output.borrow().contains(&osc52("gamma delta")));
    stop(&mut tui);
}

/// 52 `does not repaint idle or zero-width selections on focus loss`
#[test]
fn does_not_repaint_idle_or_zero_width_selections_on_focus_loss() {
    let (mut tui, output) = alt(20, 4);
    tui.add_child(Box::new(text("alpha\nbeta\ngamma\ndelta")));
    tui.start();
    tui.render_now(false);

    let idle_writes = output.borrow().len();
    tui.handle_terminal_input("\x1b[O");
    tui.handle_terminal_input("\x1b[I");
    assert_eq!(output.borrow().len(), idle_writes);

    // 完成的 click 留下零宽锚点；之后孤立的 drag/release 不得扩展它。
    tui.handle_terminal_input("\x1b[<0;1;1M");
    tui.handle_terminal_input("\x1b[<0;1;1m");
    tui.handle_terminal_input("\x1b[<32;4;2M");
    tui.handle_terminal_input("\x1b[<0;4;2m");
    assert!(!output.borrow().contains("\x1b]52;c;"));

    // 按下但未拖动后失焦：取消按下且不重绘。
    tui.handle_terminal_input("\x1b[<0;1;3M");
    let pressed_writes = output.borrow().len();
    tui.handle_terminal_input("\x1b[O");
    tui.handle_terminal_input("\x1b[I");
    assert_eq!(output.borrow().len(), pressed_writes);
    tui.handle_terminal_input("\x1b[<32;4;2M");
    tui.handle_terminal_input("\x1b[<0;4;2m");
    assert!(!output.borrow().contains("\x1b]52;c;"));
    assert!(output.borrow().contains("\x1b[?1004h"));

    stop(&mut tui);
    assert!(output.borrow().contains("\x1b[?1004l"));
}

/// 53 `clears an active visible selection on focus loss and ignores orphan events`
#[test]
fn clears_an_active_visible_selection_on_focus_loss_and_ignores_orphan_events() {
    let (mut tui, output) = alt(20, 4);
    tui.add_child(Box::new(text("alpha\nbeta\ngamma\ndelta")));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;1;1M");
    tui.handle_terminal_input("\x1b[<32;4;2M");
    tui.render_now(false);
    let before = output.borrow().len();
    tui.handle_terminal_input("\x1b[O");
    tui.handle_terminal_input("\x1b[I");
    tui.render_now(false);
    let writes = output.borrow()[before..].to_string();
    assert!(writes.contains("alpha"));
    assert!(writes.contains("beta"));
    assert!(!writes.contains("\x1b[7m"));

    tui.handle_terminal_input("\x1b[<32;4;2M");
    tui.handle_terminal_input("\x1b[<0;4;2m");
    assert!(!output.borrow().contains("\x1b]52;c;"));
    stop(&mut tui);
}

/// 54 `retains a completed visible selection across focus changes`
#[test]
fn retains_a_completed_visible_selection_across_focus_changes() {
    let (mut tui, output) = alt(20, 4);
    tui.add_child(Box::new(text("alpha\nbeta\ngamma\ndelta")));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;1;1M");
    tui.handle_terminal_input("\x1b[<32;4;2M");
    tui.handle_terminal_input("\x1b[<0;4;2m");
    tui.render_now(false);
    let completed_writes = output.borrow().len();
    tui.handle_terminal_input("\x1b[O");
    tui.handle_terminal_input("\x1b[I");
    assert_eq!(output.borrow().len(), completed_writes);

    let before = output.borrow().len();
    tui.render_now(true);
    let writes = output.borrow()[before..].to_string();
    assert!(writes.contains("alpha"));
    assert!(writes.contains("beta"));
    assert!(writes.contains("\x1b[7m"));
    stop(&mut tui);
}

/// 55 `stacks flash messages and collapses them as they expire`
#[test]
fn stacks_flash_messages_and_collapses_them_as_they_expire() {
    let (mut tui, _) = alt(20, 4);
    tui.add_child(Box::new(text("one\ntwo\nthree\nfour")));
    tui.start();
    tui.render_now(false);

    tui.flash("First", Some(80.0));
    tui.flash("Second", Some(500.0));
    tui.render_now(false);
    let viewport = screen(&tui);
    assert!(viewport[0].ends_with("First"), "{viewport:?}");
    assert!(viewport[1].ends_with("Second"), "{viewport:?}");

    std::thread::sleep(Duration::from_millis(100));
    tui.render_now(false);
    let viewport = screen(&tui);
    assert!(viewport[0].ends_with("Second"), "{viewport:?}");
    assert!(!viewport.iter().any(|line| line.contains("First")));
    stop(&mut tui);
}

/// 56 `auto-scrolls and extends a drag selection held at the viewport edge`
#[test]
fn auto_scrolls_and_extends_a_drag_selection_held_at_the_viewport_edge() {
    let (mut tui, output) = alt(20, 4);
    tui.add_child(Box::new(text(
        (1..=10)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )));
    tui.start();
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 6);

    tui.handle_terminal_input("\x1b[<0;1;3M");
    tui.handle_terminal_input("\x1b[<32;1;1M");
    std::thread::sleep(Duration::from_millis(130));
    tui.render_now(false);

    let selection_top = tui.viewport_top();
    assert!(
        selection_top < 6,
        "expected auto-scroll above row 6, got {selection_top}"
    );
    tui.handle_terminal_input("\x1b[<0;1;1m");

    let mut selected_lines: Vec<String> = (selection_top..8)
        .map(|index| format!("line {}", index + 1))
        .collect();
    selected_lines.push("l".to_string());
    assert!(
        output.borrow().contains(&osc52(&selected_lines.join("\n"))),
        "missing clipboard sequence"
    );
    stop(&mut tui);
}

/// 选择自动滚动每个 50ms tick 只渲染一帧：TS 的 `setInterval` 回调先 `scrollBy`（内部
/// `requestRender`）再 `requestRender()`，同一拍内合并成一帧（`tui-alt-screen.ts:1274-1310`）。
/// 惰性实现里 tick 的滚动发生在帧开始前，不得再排第二帧。
#[test]
fn selection_auto_scroll_tick_renders_exactly_one_frame() {
    let (mut tui, output) = alt(20, 4);
    tui.add_child(Box::new(text(
        (1..=10)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )));
    tui.start();
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 6);

    tui.handle_terminal_input("\x1b[<0;1;3M");
    tui.handle_terminal_input("\x1b[<32;1;1M");
    // 170ms = 3 个 tick（last_tick 推进到 t0+150；下一拍在 t0+200）。
    std::thread::sleep(Duration::from_millis(170));
    assert!(matches!(
        tui.render_decision(),
        kiana_tui::tui::RenderDecision::Now
    ));
    let frames_before = count_frames(&output);
    tui.render_now(false);
    assert_eq!(
        count_frames(&output),
        frames_before + 1,
        "one auto-scroll tick must write exactly one frame"
    );
    assert!(tui.viewport_top() < 6, "auto-scroll must have advanced");

    // 旧实现会因 scrollBy 的重绘请求在帧内重新置起重绘标志，多排一帧。
    assert!(
        !tui.render_requested_handle().get(),
        "auto-scroll tick must not schedule a second frame"
    );
    stop(&mut tui);
}

/// 57 `snaps mouse selection to CJK, emoji, and combining grapheme boundaries`
#[test]
fn snaps_mouse_selection_to_cjk_emoji_and_combining_grapheme_boundaries() {
    let (mut tui, output) = alt(20, 2);
    tui.add_child(Box::new(text("A界🙂e\u{301}Z")));
    tui.start();
    tui.render_now(false);

    let wide_selection = osc52("界🙂");
    tui.handle_terminal_input("\x1b[<0;3;1M");
    tui.handle_terminal_input("\x1b[<32;4;1M");
    tui.handle_terminal_input("\x1b[<0;4;1m");
    tui.render_now(false);
    assert_eq!(output.borrow().matches(&wide_selection).count(), 1);

    tui.handle_terminal_input("\x1b[<0;5;1M");
    tui.handle_terminal_input("\x1b[<32;2;1M");
    tui.handle_terminal_input("\x1b[<0;2;1m");
    tui.render_now(false);
    assert_eq!(output.borrow().matches(&wide_selection).count(), 2);

    let combining_selection = osc52("e\u{301}Z");
    tui.handle_terminal_input("\x1b[<0;6;1M");
    tui.handle_terminal_input("\x1b[<32;7;1M");
    tui.handle_terminal_input("\x1b[<0;7;1m");
    tui.render_now(false);
    assert!(output.borrow().contains(&combining_selection));
    stop(&mut tui);
}

// ---------------------------------------------------------------------------
// 鼠标分发（58-68）
// ---------------------------------------------------------------------------

/// 58 `ignores horizontal trackpad wheel events`
#[test]
fn ignores_horizontal_trackpad_wheel_events() {
    let (mut tui, _) = alt(20, 4);
    tui.add_child(Box::new(text(
        (1..=8)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<66;1;1M");
    tui.handle_terminal_input("\x1b[<67;1;1M");
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 4);
    assert_screen(&tui, &["line 5", "line 6", "line 7", "line 8"]);
    stop(&mut tui);
}

/// 59 `dispatches clicks to nested mouse regions without breaking drag selection`
#[test]
fn dispatches_clicks_to_nested_mouse_regions_without_breaking_drag_selection() {
    let (mut tui, output) = alt(20, 2);
    let clicks = Rc::new(Cell::new(0usize));
    let counter = Rc::clone(&clicks);
    tui.add_child(Box::new(MouseRegion::new(
        Box::new(text("clickable\nselectable")),
        Box::new(move |event: &TuiMouseEvent| {
            if event.event_type != TuiMouseEventType::Click {
                return None;
            }
            counter.set(counter.get() + 1);
            Some(TuiMouseEventResult {
                handled: Some(true),
                ..Default::default()
            })
        }),
    )));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;2;1M");
    tui.handle_terminal_input("\x1b[<0;2;1m");
    tui.render_now(false);
    assert_eq!(clicks.get(), 1);

    tui.handle_terminal_input("\x1b[<0;1;1M");
    tui.handle_terminal_input("\x1b[<32;4;2M");
    tui.handle_terminal_input("\x1b[<0;4;2m");
    tui.render_now(false);
    assert_eq!(clicks.get(), 1);
    assert!(output.borrow().contains("\x1b]52;c;"));
    stop(&mut tui);
}

/// 60 `scrolls the transcript on wheel input a focused overlay does not handle`
#[test]
fn scrolls_the_transcript_on_wheel_input_a_focused_overlay_does_not_handle() {
    let (mut tui, _) = alt(20, 6);
    tui.add_child(Box::new(text(
        (1..=12)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )));
    let overlay = InputOverlay::new();
    let overlay_inputs = Rc::clone(&overlay.inputs);
    let overlay_focused = Rc::clone(&overlay.focused);
    tui.start();
    tui.render_now(false);
    let top_before = tui.viewport_top();
    tui.show_overlay(
        Box::new(overlay),
        OverlayOptions {
            anchor: Some(OverlayAnchor::TopLeft),
            width: Some(SizeValue::Absolute(10.0)),
            ..Default::default()
        },
    );
    tui.render_now(false);
    assert!(overlay_focused.get());

    tui.handle_terminal_input("\x1b[<64;3;1M");
    tui.render_now(false);
    assert!(tui.viewport_top() < top_before);
    assert!(overlay_inputs.borrow().is_empty());
    stop(&mut tui);
}

/// 61 `focuses and captures drag gestures for mouse-aware components`
#[test]
fn focuses_and_captures_drag_gestures_for_mouse_aware_components() {
    let (mut tui, _) = alt(20, 2);
    let events: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let recorded = Rc::clone(&events);
    struct Control {
        events: Rc<RefCell<Vec<String>>>,
    }
    impl Component for Control {
        fn render(&mut self, _width: usize) -> Vec<String> {
            vec!["control".to_string()]
        }
        fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
            self.events
                .borrow_mut()
                .push(format!("{:?}", event.event_type).to_lowercase());
            if event.event_type == TuiMouseEventType::Press {
                Some(TuiMouseEventResult {
                    handled: Some(true),
                    capture: Some(true),
                    focus: Some(true),
                    ..Default::default()
                })
            } else {
                Some(TuiMouseEventResult {
                    handled: Some(true),
                    ..Default::default()
                })
            }
        }
    }
    let control = shared(Control {
        events: Rc::clone(&recorded),
    });
    tui.add_child_shared(as_shared(&control));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;1;1M");
    tui.handle_terminal_input("\x1b[<32;5;2M");
    tui.handle_terminal_input("\x1b[<0;5;2m");
    tui.render_now(false);

    assert_eq!(
        *events.borrow(),
        vec![
            "press".to_string(),
            "drag".to_string(),
            "release".to_string()
        ]
    );
    let expected = tui
        .focus_target_of_component(&as_shared(&control))
        .expect("control mounted");
    assert_eq!(tui.focus(), Some(expected));
    stop(&mut tui);
}

/// 62 `reports consecutive click counts to component-owned controls`
#[test]
fn reports_consecutive_click_counts_to_component_owned_controls() {
    let (mut tui, _) = alt(20, 1);
    let counts: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));
    let recorded = Rc::clone(&counts);
    struct Control {
        counts: Rc<RefCell<Vec<u32>>>,
    }
    impl Component for Control {
        fn render(&mut self, _width: usize) -> Vec<String> {
            vec!["control".to_string()]
        }
        fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
            match event.event_type {
                TuiMouseEventType::Press => Some(TuiMouseEventResult {
                    handled: Some(true),
                    ..Default::default()
                }),
                TuiMouseEventType::Click => {
                    self.counts
                        .borrow_mut()
                        .push(event.click_count.unwrap_or(0));
                    Some(TuiMouseEventResult {
                        handled: Some(true),
                        ..Default::default()
                    })
                }
                _ => None,
            }
        }
    }
    tui.add_child(Box::new(Control {
        counts: Rc::clone(&recorded),
    }));
    tui.start();
    tui.render_now(false);

    for _ in 0..3 {
        tui.handle_terminal_input("\x1b[<0;1;1M");
        tui.handle_terminal_input("\x1b[<0;1;1m");
    }
    tui.render_now(false);
    assert_eq!(*counts.borrow(), vec![1, 2, 3]);
    stop(&mut tui);
}

/// 63 `does not rerender for handled no-op pointer motion`
#[test]
fn does_not_rerender_for_handled_no_op_pointer_motion() {
    let (mut tui, output) = alt(20, 2);
    let render_count = Rc::new(Cell::new(0usize));
    let counter = Rc::clone(&render_count);
    struct HoverTarget {
        render_count: Rc<Cell<usize>>,
    }
    impl Component for HoverTarget {
        fn render(&mut self, _width: usize) -> Vec<String> {
            self.render_count.set(self.render_count.get() + 1);
            vec!["hover target".to_string()]
        }
        fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
            (event.event_type == TuiMouseEventType::Move).then(|| TuiMouseEventResult {
                handled: Some(true),
                ..Default::default()
            })
        }
    }
    tui.add_child(Box::new(HoverTarget {
        render_count: Rc::clone(&counter),
    }));
    tui.start();
    tui.render_now(false);
    let rendered_before = render_count.get();
    let writes_before = output.borrow().len();

    tui.handle_terminal_input("\x1b[<35;1;1M");
    assert_eq!(render_count.get(), rendered_before);
    assert_eq!(output.borrow().len(), writes_before);
    stop(&mut tui);
}

/// 64 `lets mouse-aware components consume wheel events before viewport scrolling`
#[test]
fn lets_mouse_aware_components_consume_wheel_events_before_viewport_scrolling() {
    let (mut tui, _) = alt(20, 3);
    let wheel_events = Rc::new(Cell::new(0usize));
    let counter = Rc::clone(&wheel_events);
    tui.add_child(Box::new(MouseRegion::new(
        Box::new(text(
            (1..=8)
                .map(|index| format!("line {index}"))
                .collect::<Vec<_>>()
                .join("\n"),
        )),
        Box::new(move |event: &TuiMouseEvent| {
            if event.event_type != TuiMouseEventType::Wheel {
                return None;
            }
            counter.set(counter.get() + 1);
            Some(TuiMouseEventResult {
                handled: Some(true),
                ..Default::default()
            })
        }),
    )));
    tui.start();
    tui.render_now(false);
    let viewport_top = tui.viewport_top();

    tui.handle_terminal_input("\x1b[<64;1;1M");
    tui.render_now(false);
    assert_eq!(wheel_events.get(), 1);
    assert_eq!(tui.viewport_top(), viewport_top);
    stop(&mut tui);
}

/// 65 `restores keyboard state before leaving alt mode and prints the full document`
#[test]
fn restores_keyboard_state_before_leaving_alt_mode_and_prints_the_full_document() {
    let (mut tui, output) = alt(20, 3);
    tui.add_child(Box::new(text("first\nsecond\nthird\nfourth\nfifth\nsixth")));
    tui.start();
    tui.render_now(false);
    stop(&mut tui);
    let writes = output.borrow().clone();

    let alt_screen_enter = writes.find("\x1b[?1049h").expect("alt enter");
    let start = writes.find("\x1b[?2004h").expect("terminal start");
    let mouse_disable = writes.find("\x1b[?1006l").expect("mouse disable");
    let stop = writes.find("\x1b[?2004l").expect("terminal stop");
    let main_screen_restore = writes.find("\x1b[?1049l").expect("alt exit");
    assert!(alt_screen_enter < start);
    assert!(mouse_disable < stop);
    assert!(main_screen_restore > stop);

    let restore = &writes[main_screen_restore..];
    for line in ["first", "second", "third", "fourth", "fifth", "sixth"] {
        assert!(restore.contains(line), "restore missing {line}");
    }
    assert!(restore.find("first").unwrap() < restore.find("sixth").unwrap());
}

/// 66 `gives viewport keys to a focused overlay`
#[test]
fn gives_viewport_keys_to_a_focused_overlay() {
    let (mut tui, _) = alt(20, 6);
    tui.add_child(Box::new(text(
        (1..=12)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )));
    let overlay = InputOverlay::new();
    let overlay_inputs = Rc::clone(&overlay.inputs);
    let overlay_focused = Rc::clone(&overlay.focused);
    tui.start();
    tui.render_now(false);
    let top_before = tui.viewport_top();
    let mut handle = tui.show_overlay(Box::new(overlay), Default::default());
    tui.render_now(false);
    assert!(overlay_focused.get());

    let keys = ["\x1b[5~", "\x1b[6~", "\x1bOH", "\x1bOF"];
    for key in keys {
        tui.handle_terminal_input(key);
    }
    tui.render_now(false);

    assert_eq!(
        *overlay_inputs.borrow(),
        keys.iter().map(|key| key.to_string()).collect::<Vec<_>>()
    );
    assert_eq!(tui.viewport_top(), top_before);

    handle.hide();
    tui.render_now(false);
    tui.handle_terminal_input("\x1b[5~");
    tui.render_now(false);
    assert!(tui.viewport_top() < top_before);
    stop(&mut tui);
}

/// 67 `keeps viewport scrolling when an overlay is not focused`
#[test]
fn keeps_viewport_scrolling_when_an_overlay_is_not_focused() {
    let (mut tui, _) = alt(20, 6);
    let editor = shared({
        struct Editor {
            inputs: Rc<RefCell<Vec<String>>>,
        }
        impl Component for Editor {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec!["editor".to_string()]
            }
            fn handle_input(&mut self, data: &str) {
                self.inputs.borrow_mut().push(data.to_string());
            }
        }
        Editor {
            inputs: Rc::new(RefCell::new(Vec::new())),
        }
    });
    tui.add_child_shared(as_shared(&editor));
    tui.add_child(Box::new(text(
        (1..=12)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )));
    focus_shared(&mut tui, &as_shared(&editor));
    tui.start();
    tui.render_now(false);
    let top_before = tui.viewport_top();

    let mut hidden = tui.show_overlay(Box::new(InputOverlay::new()), Default::default());
    hidden.set_hidden(true);
    let non_capturing = InputOverlay::new();
    let non_capturing_inputs = Rc::clone(&non_capturing.inputs);
    let non_capturing_focused = Rc::clone(&non_capturing.focused);
    tui.show_overlay(
        Box::new(non_capturing),
        OverlayOptions {
            non_capturing: Some(true),
            ..Default::default()
        },
    );
    let unfocused = InputOverlay::new();
    let unfocused_inputs = Rc::clone(&unfocused.inputs);
    let unfocused_focused = Rc::clone(&unfocused.focused);
    let mut unfocused_handle = tui.show_overlay(Box::new(unfocused), Default::default());
    unfocused_handle.unfocus(None);
    tui.render_now(false);
    assert!(!non_capturing_focused.get());
    assert!(!unfocused_focused.get());

    tui.handle_terminal_input("\x1b[5~");
    tui.handle_terminal_input("\x1b[<64;10;3M");
    tui.render_now(false);
    assert!(tui.viewport_top() < top_before);
    assert!(non_capturing_inputs.borrow().is_empty());
    assert!(unfocused_inputs.borrow().is_empty());
    stop(&mut tui);
}

/// 68 `keeps viewport scrolling while transcript search is focused`
#[test]
fn keeps_viewport_scrolling_while_transcript_search_is_focused() {
    let (mut tui, _) = alt(20, 6);
    tui.add_child(Box::new(text(
        (1..=12)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )));
    tui.start();
    tui.render_now(false);
    let top_before = tui.viewport_top();

    tui.handle_terminal_input("\x1b[102;6u");
    tui.render_now(false);
    assert!(padded_screen(&tui).iter().any(|line| line.contains("↑ ↓")));

    tui.handle_terminal_input("\x1b[5~");
    tui.handle_terminal_input("\x1b[<64;1;4M");
    tui.render_now(false);
    assert!(tui.viewport_top() < top_before);
    assert!(padded_screen(&tui).iter().any(|line| line.contains("↑ ↓")));
    stop(&mut tui);
}

// ---------------------------------------------------------------------------
// 退出恢复（`test/exit-restore.test.ts` 里 TuiAltScreen 的契约）
// ---------------------------------------------------------------------------

/// 未 `stop()` 就退出：`restoreOnExit(before, after)` 必须带上备用屏的恢复序列
/// （TS `getExitRestorePrefix/Suffix` 覆写；见 `spec/tui-core.md` §2.13.4）。
#[test]
fn alt_screen_exit_restore_prefix_and_suffix() {
    let terminal = TestTerminal::with_size(20, 4);
    let calls = terminal.restore_calls_handle();
    let mut tui = TuiAltScreen::new(
        Box::new(terminal),
        None,
        None,
        TuiAltScreenOptions::default(),
    );
    tui.add_child(Box::new(text("content")));
    tui.start();
    tui.render_now(false);
    tui.run_exit_restore_handler();
    let calls = calls.borrow();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0],
        (
            "\x1b[?2026l\x1b[?1006l\x1b[?1004l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?7h\x1b[?25h"
                .to_string(),
            "\x1b[?1049l\x1b[?25h".to_string()
        )
    );
}

// ---------------------------------------------------------------------------
// 补充：模块契约（TS 里由类型系统/隐式行为覆盖的部分）
// ---------------------------------------------------------------------------

/// 空/纯空白 query 不匹配任何东西（`findAltScreenSearchMatches` 早退，不构建语料）。
#[test]
fn search_empty_or_whitespace_query_matches_nothing() {
    use kiana_tui::alt_screen_search::find_alt_screen_search_matches;
    assert!(find_alt_screen_search_matches(&["alpha".to_string()], "").is_empty());
    assert!(find_alt_screen_search_matches(&["alpha".to_string()], "   \t ").is_empty());
    // 归一化后的普通查询（多空格折叠）。
    assert_eq!(
        find_alt_screen_search_matches(&["alpha   beta".to_string()], "  alpha beta ").len(),
        1
    );
}

/// 搜索高亮只给普通文本段加样式，ANSI 代码原样保留（`applySearchTextHighlight`）。
#[test]
fn search_highlight_wraps_only_plain_runs() {
    let (mut tui, output) = alt(40, 6);
    tui.add_child(Box::new(text("\x1b[31mneedle\x1b[0m tail")));
    tui.start();
    tui.render_now(false);
    tui.handle_terminal_input("\x1b[102;6u");
    tui.handle_terminal_input("needle");
    tui.render_now(false);
    // 当前匹配用 `searchCurrentMatchStyle`（默认 `\x1b[1;7m…\x1b[22;27m`）；切片还会在匹配后
    // 重新打开原前景色（`\x1b[31m`）以保持外层样式。
    assert!(
        output
            .borrow()
            .contains("\x1b[31m\x1b[1;7mneedle\x1b[22;27m\x1b[31m\x1b[0m"),
        "writes: {:?}",
        output.borrow()
    );
    stop(&mut tui);
}

/// X10 编码的滚轮事件（6 字节 `ESC [ M` + 3 个字符）同样被路由。
#[test]
fn routes_x10_encoded_wheel_events() {
    let (mut tui, _) = alt(20, 4);
    tui.add_child(Box::new(text(
        (1..=10)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )));
    tui.start();
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 6);
    // button = 96 - 32 = 64（滚轮上），x = 33 - 33 = 0，y = 33 - 33 = 0。
    let sequence = format!("\x1b[M{}{}{}", 96u8 as char, 33u8 as char, 33u8 as char);
    tui.handle_terminal_input(&sequence);
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 5);
    stop(&mut tui);
}

/// `mode` / `isViewportTUI` / `Component::render`（布局根优先，否则子组件）。
#[test]
fn mode_viewport_marker_and_render_contract() {
    let (mut tui, _) = alt(10, 3);
    tui.add_child(Box::new(text("child")));
    assert_eq!(tui.mode(), TuiMode::Fullscreen);
    assert!(tui.is_viewport_tui());
    assert_eq!(
        tui.render(10)
            .iter()
            .map(|line| strip_terminal_sequences(line).trim_end().to_string())
            .collect::<Vec<_>>(),
        vec!["child".to_string()]
    );

    tui.set_layout_root_component(Some(Box::new(text("root"))));
    assert_eq!(
        tui.render(10)
            .iter()
            .map(|line| strip_terminal_sequences(line).trim_end().to_string())
            .collect::<Vec<_>>(),
        vec!["root".to_string()]
    );
    tui.set_layout_root_component(None);
    assert_eq!(
        tui.render(10)
            .iter()
            .map(|line| strip_terminal_sequences(line).trim_end().to_string())
            .collect::<Vec<_>>(),
        vec!["child".to_string()]
    );
    stop(&mut tui);
}

/// copyOnSelect 存取与 `resetTextSelection`。
#[test]
fn copy_on_select_accessors_and_reset_text_selection() {
    let (mut tui, _) = alt(20, 4);
    tui.add_child(Box::new(text("alpha\nbeta\ngamma\ndelta")));
    tui.start();
    tui.render_now(false);
    assert!(tui.get_copy_on_select());
    tui.set_copy_on_select(false);
    assert!(!tui.get_copy_on_select());

    tui.handle_terminal_input("\x1b[<0;1;1M");
    tui.handle_terminal_input("\x1b[<32;4;2M");
    tui.handle_terminal_input("\x1b[<0;4;2m");
    assert!(tui.has_active_selection());
    tui.reset_text_selection();
    assert!(!tui.has_active_selection());
    stop(&mut tui);
}

/// `pump_pending_input` 逐条分发 `Terminal::start` 回调排进队列的输入。
#[test]
fn pump_pending_input_dispatches_queued_terminal_input() {
    let terminal = Rc::new(RefCell::new(TestTerminal::with_size(20, 4)));
    let mut tui = TuiAltScreen::new(
        Box::new(SharedTestTerminal(Rc::clone(&terminal))),
        None,
        None,
        TuiAltScreenOptions::default(),
    );
    tui.add_child(Box::new(text(
        (1..=10)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )));
    tui.start();
    tui.render_now(false);
    let top = tui.viewport_top();

    // `sendInput` 只触发 `Terminal::start` 的回调（数据进队列），尚未派发。
    terminal.borrow_mut().send_input("\x1b[<64;1;1M");
    assert_eq!(
        tui.viewport_top(),
        top,
        "input stays queued until the host pumps"
    );

    tui.pump_pending_input();
    assert_eq!(tui.viewport_top(), top - 1);
    stop(&mut tui);
}

// ---------------------------------------------------------------------------
// 评审回归（TS 与 Rust 的行为差异修复；期望值用 TS 参考实现逐字节生成）
// ---------------------------------------------------------------------------

/// 记录键盘输入的可聚焦组件（TS 的 Editor 替身；`focused` 可观察）。
struct FocusEditor {
    inputs: Rc<RefCell<Vec<String>>>,
    focused: Rc<Cell<bool>>,
}

impl FocusEditor {
    fn new() -> Self {
        FocusEditor {
            inputs: Rc::new(RefCell::new(Vec::new())),
            focused: Rc::new(Cell::new(false)),
        }
    }
}

impl Component for FocusEditor {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec!["editor".to_string()]
    }
    fn handle_input(&mut self, data: &str) {
        self.inputs.borrow_mut().push(data.to_string());
    }
    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for FocusEditor {
    fn focused(&self) -> bool {
        self.focused.get()
    }
    fn set_focused(&mut self, focused: bool) {
        self.focused.set(focused);
    }
}

/// 共享的 [`TestTerminal`]：把终端交给 TUI 之后，测试仍能通过 `send_input` 注入输入
/// （驱动 `Terminal::start` 注册的回调 → `TuiBase` 的输入队列 → `pump_pending_input`）。
struct SharedTestTerminal(Rc<RefCell<TestTerminal>>);

impl kiana_tui::terminal_colors::Terminal for SharedTestTerminal {
    fn restore_on_exit(&mut self, before: &str, after: &str) {
        self.0.borrow_mut().restore_on_exit(before, after);
    }
    fn start(&mut self, on_input: Box<dyn FnMut(&str)>, on_resize: Box<dyn FnMut()>) {
        self.0.borrow_mut().start(on_input, on_resize);
    }
    fn stop(&mut self) {
        self.0.borrow_mut().stop();
    }
    fn drain_input(
        &mut self,
        _max_ms: u64,
        _idle_ms: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + '_>> {
        Box::pin(async {})
    }
    fn write(&mut self, data: &str) {
        self.0.borrow_mut().write(data);
    }
    fn columns(&self) -> usize {
        self.0.borrow().columns()
    }
    fn rows(&self) -> usize {
        self.0.borrow().rows()
    }
    fn kitty_protocol_active(&self) -> bool {
        self.0.borrow().kitty_protocol_active()
    }
    fn move_by(&mut self, lines: isize) {
        self.0.borrow_mut().move_by(lines);
    }
    fn hide_cursor(&mut self) {
        self.0.borrow_mut().hide_cursor();
    }
    fn show_cursor(&mut self) {
        self.0.borrow_mut().show_cursor();
    }
    fn clear_line(&mut self) {
        self.0.borrow_mut().clear_line();
    }
    fn clear_from_cursor(&mut self) {
        self.0.borrow_mut().clear_from_cursor();
    }
    fn clear_screen(&mut self) {
        self.0.borrow_mut().clear_screen();
    }
    fn set_title(&mut self, title: &str) {
        self.0.borrow_mut().set_title(title);
    }
    fn set_progress(&mut self, active: bool) {
        self.0.borrow_mut().set_progress(active);
    }
    fn set_program_status(&mut self, status: &kiana_tui::terminal_colors::ProgramStatus) {
        self.0.borrow_mut().set_program_status(status);
    }
}

/// 驱动宿主渲染循环直到 `Idle`（`Wait` 表示等定时器/节流窗口到点后重新查询）。
fn render_until_idle(tui: &mut TuiAltScreen) -> usize {
    let mut frames = 0usize;
    loop {
        match tui.render_decision() {
            kiana_tui::tui::RenderDecision::Idle => return frames,
            kiana_tui::tui::RenderDecision::Now => {
                tui.render_now(false);
                frames += 1;
            }
            kiana_tui::tui::RenderDecision::Wait(ms) => {
                std::thread::sleep(Duration::from_millis(ms.max(0.0).ceil() as u64));
            }
        }
        assert!(frames < 100, "render loop did not settle");
    }
}

/// 终端输出里的帧数：`doRender` 每次写一个以 `BEGIN_SYNCHRONIZED_OUTPUT` 开头的缓冲。
fn count_frames(output: &Rc<RefCell<String>>) -> usize {
    output.borrow().matches("\x1b[?2026h").count()
}

/// 内部重绘请求（滚动条滚动/悬停、flash 到期、滚动条自动隐藏）必须唤醒宿主。
///
/// TS 里这些路径都以 `requestRender()` 结尾（`ScrollView` 的 `requestRenderCallback`、
/// `AltScreenFlashContainer` 的 `setTimeout`、滚动条隐藏的 `setTimeout`）；Rust 的宿主
/// 只通过 [`TuiAltScreen::render_decision`] 查询调度，因此这里按宿主循环的方式断言。
#[test]
fn scrollbar_track_click_requests_a_frame() {
    let (mut tui, _) = alt(10, 10);
    let scroll_view = shared(
        ScrollView::new(
            Box::new(text(
                (1..=50)
                    .map(|index| format!("line {index}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
            ScrollViewOptions {
                primary: Some(true),
                scrollbar: Some(ScrollViewScrollbar::Always),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    tui.set_layout_root_component(Some(Box::new(SharedRef(as_shared(&scroll_view)))));
    tui.start();
    render_until_idle(&mut tui);
    assert_eq!(scroll_view.borrow().scroll_top(), 0);

    tui.handle_terminal_input("\x1b[<0;10;6M");
    assert_eq!(scroll_view.borrow().scroll_top(), 20);
    assert!(
        !matches!(tui.render_decision(), kiana_tui::tui::RenderDecision::Idle),
        "scrollbar track click must schedule a frame"
    );
    render_until_idle(&mut tui);
    assert!(screen(&tui)[0].starts_with("line 21"), "{:?}", screen(&tui));
    stop(&mut tui);
}

/// `flash` 的 `setTimeout` 到期后必须让宿主渲染一帧（移除过期消息）。
#[test]
fn flash_expiry_requests_a_frame() {
    let (mut tui, _) = alt(20, 4);
    tui.add_child(Box::new(text("one\ntwo\nthree\nfour")));
    tui.start();
    render_until_idle(&mut tui);

    tui.flash("Hi", Some(40.0));
    tui.render_now(false);
    assert!(screen(&tui)[0].ends_with("Hi"), "{:?}", screen(&tui));
    std::thread::sleep(Duration::from_millis(60));
    assert!(
        matches!(tui.render_decision(), kiana_tui::tui::RenderDecision::Now),
        "flash deadline must wake the host"
    );
    tui.render_now(false);
    assert!(!screen(&tui).iter().any(|line| line.contains("Hi")));
    stop(&mut tui);
}

/// flash 到期**只产生一帧**：TS 的 `setTimeout` 回调删除条目后只 `requestRender()` 一次
/// （`alt-screen-flash.ts:16-32`）。结算发生在这一帧里，不得再排一次空帧。
#[test]
fn flash_expiry_renders_exactly_one_frame() {
    let (mut tui, output) = alt(20, 4);
    tui.add_child(Box::new(text("one\ntwo\nthree\nfour")));
    tui.start();
    tui.render_now(false);
    assert!(matches!(
        tui.render_decision(),
        kiana_tui::tui::RenderDecision::Idle
    ));

    tui.flash("Hi", Some(40.0));
    tui.render_now(false);
    assert!(screen(&tui)[0].ends_with("Hi"), "{:?}", screen(&tui));
    let frames_before = count_frames(&output);

    std::thread::sleep(Duration::from_millis(60));
    assert!(matches!(
        tui.render_decision(),
        kiana_tui::tui::RenderDecision::Now
    ));
    tui.render_now(false);
    assert!(!screen(&tui).iter().any(|line| line.contains("Hi")));
    assert_eq!(
        count_frames(&output),
        frames_before + 1,
        "flash expiry must render exactly one frame"
    );

    // 结算本身不得再请求一帧（TS 定时器回调只 requestRender 一次）。
    assert!(
        !tui.render_requested_handle().get(),
        "flash expiry must not schedule a second (empty) frame"
    );
    stop(&mut tui);
}

/// 自动隐藏滚动条的 `setTimeout` 到期后宿主被唤醒并重绘（隐藏 thumb）。
#[test]
fn auto_scrollbar_hide_timer_wakes_the_host() {
    let (mut tui, _) = alt(10, 5);
    let scroll_view = shared(
        ScrollView::new(
            Box::new(text(
                (1..=20)
                    .map(|index| format!("line {index}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
            ScrollViewOptions {
                primary: Some(true),
                scrollbar: Some(ScrollViewScrollbar::Auto),
                scrollbar_hide_delay_ms: Some(20.0),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    tui.set_layout_root_component(Some(Box::new(SharedRef(as_shared(&scroll_view)))));
    tui.start();
    render_until_idle(&mut tui);

    tui.handle_terminal_input("\x1b[<35;10;3M");
    render_until_idle(&mut tui);
    assert!(scroll_view.borrow().is_scrollbar_visible());

    tui.handle_terminal_input("\x1b[<35;9;3M");
    // 宿主循环会在隐藏定时器到点后自动渲染，无需测试手动 render_now。
    render_until_idle(&mut tui);
    assert!(!scroll_view.borrow().is_scrollbar_visible());
    stop(&mut tui);
}

/// 滚动条自动隐藏在**直接渲染**路径上同样只产生一帧：TS 的 `setTimeout` 回调 `requestRender()`
/// 一次；惰性结算在帧开始前完成，`ScrollView::render` 不得再补发重绘请求。
#[test]
fn scrollbar_hide_expiry_renders_exactly_one_frame() {
    let (mut tui, _) = alt(10, 5);
    let scroll_view = shared(
        ScrollView::new(
            Box::new(text(
                (1..=20)
                    .map(|index| format!("line {index}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
            ScrollViewOptions {
                primary: Some(true),
                scrollbar: Some(ScrollViewScrollbar::Auto),
                scrollbar_hide_delay_ms: Some(20.0),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    tui.set_layout_root_component(Some(Box::new(SharedRef(as_shared(&scroll_view)))));
    tui.start();
    render_until_idle(&mut tui);

    // 真正的滚轮事件（button 65 = wheel down），指针停在内容列（不是滚动条轨道列），
    // 否则悬停会让瞬时滚动条保持可见。
    tui.handle_terminal_input("\x1b[<65;9;3M");
    tui.render_now(false);
    assert!(scroll_view.borrow().is_scrollbar_visible());

    std::thread::sleep(Duration::from_millis(40));
    // 绕过 render_decision 直接渲染：帧内结算过期定时器，但不得再排一帧。
    tui.render_now(false);
    assert!(!scroll_view.borrow().is_scrollbar_visible());
    assert!(
        !tui.render_requested_handle().get(),
        "scrollbar hide expiry must not schedule a second frame"
    );
    stop(&mut tui);
}

/// 选择整体滚出视口上方时不得在首可见行留下"幽灵高亮"。
///
/// 期望帧由 TS 参考实现生成（20x4、10 行、无 copyOnSelect）。
#[test]
fn selection_above_the_viewport_does_not_paint_the_first_visible_row() {
    let (mut tui, _) = alt_with(
        20,
        4,
        TuiAltScreenOptions {
            copy_on_select: Some(false),
            ..Default::default()
        },
    );
    tui.add_child(Box::new(text(
        (1..=10)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )));
    tui.start();
    tui.render_now(false);
    tui.scroll_to_top();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;3;1M");
    tui.handle_terminal_input("\x1b[<32;6;3M");
    tui.handle_terminal_input("\x1b[<0;6;3m");
    tui.render_now(false);
    assert_eq!(
        tui.get_screen_lines(),
        vec![
            "li\x1b[7mne 1              \x1b[27m\x1b[0m\x1b]8;;\x07".to_string(),
            "\x1b[7mline 2              \x1b[27m\x1b[0m\x1b]8;;\x07".to_string(),
            "\x1b[7mline 3\x1b[27m              \x1b[0m\x1b]8;;\x07".to_string(),
            "line 4              \x1b[0m\x1b]8;;\x07".to_string(),
        ]
    );

    tui.handle_terminal_input("\x1b[<65;1;1M");
    tui.handle_terminal_input("\x1b[<65;1;1M");
    tui.handle_terminal_input("\x1b[<65;1;1M");
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 3);
    assert_eq!(
        tui.get_screen_lines(),
        vec![
            "line 4              \x1b[0m\x1b]8;;\x07".to_string(),
            "line 5              \x1b[0m\x1b]8;;\x07".to_string(),
            "line 6              \x1b[0m\x1b]8;;\x07".to_string(),
            "line 7              \x1b[0m\x1b]8;;\x07".to_string(),
        ],
        "selection fully above the viewport must not paint row 0"
    );
    stop(&mut tui);
}

/// 选择跨入视口时，起始行之后的可见行从第 0 列整行高亮；这是当前渲染的稳定输出，改动需同步测试。
#[test]
fn selection_crossing_into_the_viewport_highlights_from_column_zero() {
    let (mut tui, _) = alt_with(
        20,
        4,
        TuiAltScreenOptions {
            copy_on_select: Some(false),
            ..Default::default()
        },
    );
    tui.add_child(Box::new(text(
        (1..=10)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )));
    tui.start();
    tui.render_now(false);
    tui.scroll_to_top();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;3;1M");
    tui.handle_terminal_input("\x1b[<32;9;6M");
    tui.handle_terminal_input("\x1b[<0;9;6m");
    tui.render_now(false);
    assert_eq!(
        tui.get_screen_lines(),
        vec![
            "li\x1b[7mne 1              \x1b[27m\x1b[0m\x1b]8;;\x07".to_string(),
            "\x1b[7mline 2              \x1b[27m\x1b[0m\x1b]8;;\x07".to_string(),
            "\x1b[7mline 3              \x1b[27m\x1b[0m\x1b]8;;\x07".to_string(),
            "\x1b[7mline 4   \x1b[27m           \x1b[0m\x1b]8;;\x07".to_string(),
        ]
    );

    tui.handle_terminal_input("\x1b[<65;1;1M");
    tui.handle_terminal_input("\x1b[<65;1;1M");
    tui.handle_terminal_input("\x1b[<65;1;1M");
    tui.render_now(false);
    assert_eq!(tui.viewport_top(), 3);
    assert_eq!(
        tui.get_screen_lines(),
        vec![
            "\x1b[7mline 4   \x1b[27m           \x1b[0m\x1b]8;;\x07".to_string(),
            "line 5              \x1b[0m\x1b]8;;\x07".to_string(),
            "line 6              \x1b[0m\x1b]8;;\x07".to_string(),
            "line 7              \x1b[0m\x1b]8;;\x07".to_string(),
        ],
        "first visible row of a crossing selection must be highlighted from column 0"
    );
    stop(&mut tui);
}

/// `Tui::children` 是挂载列表本身：`switchTuiMode` 用 `[...previousUi.children]` 迁移组件。
#[test]
fn children_is_the_mounted_child_list_and_components_can_move_between_tuis() {
    let (mut tui, _) = alt(20, 4);
    tui.add_child(Box::new(text("first")));
    tui.add_child(Box::new(text("second")));
    tui.start();
    tui.render_now(false);
    assert_eq!(tui.children().len(), 2);
    assert_eq!(&screen(&tui)[..2], ["first", "second"]);

    let components: Vec<Box<dyn Component>> = std::mem::take(tui.children());
    tui.render_now(false);
    assert!(screen(&tui).iter().all(|line| line.trim().is_empty()));

    let (mut next, _) = alt(20, 4);
    for component in components {
        next.add_child(component);
    }
    next.start();
    next.render_now(false);
    assert_eq!(&screen(&next)[..2], ["first", "second"]);
    stop(&mut tui);
    stop(&mut next);
}

/// TS `removeChild` 不动焦点：被删组件继续接收输入，其他组件保持未聚焦。
#[test]
fn removing_a_focused_child_keeps_input_on_the_removed_component() {
    let (mut tui, _) = alt(20, 4);
    let a = shared(FocusEditor::new());
    let b = shared(FocusEditor::new());
    tui.add_child_shared(as_shared(&a));
    tui.add_child_shared(as_shared(&b));
    tui.start();
    tui.render_now(false);
    focus_shared(&mut tui, &as_shared(&a));

    tui.remove_child(&*a.borrow());
    assert_eq!(tui.children().len(), 1);
    tui.handle_terminal_input("y");
    assert_eq!(*a.borrow().inputs.borrow(), vec!["y".to_string()]);
    assert!(b.borrow().inputs.borrow().is_empty());
    assert!(a.borrow().focused.get(), "removed component keeps focused");
    assert!(!b.borrow().focused.get(), "sibling must stay unfocused");

    // 身份比较不得借用组件内部状态：持有 A 的 `borrow_mut` 时删除 B 不应 panic。
    let held = a.borrow_mut();
    tui.remove_child(&*b.borrow());
    drop(held);
    stop(&mut tui);
}

/// 删除聚焦组件之前的兄弟后，焦点仍指向同一组件（TS 的引用语义；Rust 下标需重映射）。
#[test]
fn removing_an_earlier_sibling_keeps_focus_on_the_same_component() {
    let (mut tui, _) = alt(20, 4);
    let a = shared(FocusEditor::new());
    let b = shared(FocusEditor::new());
    tui.add_child_shared(as_shared(&a));
    tui.add_child_shared(as_shared(&b));
    tui.start();
    tui.render_now(false);
    focus_shared(&mut tui, &as_shared(&b));

    tui.remove_child(&*a.borrow());
    tui.handle_terminal_input("z");
    assert_eq!(*b.borrow().inputs.borrow(), vec!["z".to_string()]);
    assert!(a.borrow().inputs.borrow().is_empty());
    stop(&mut tui);
}

/// `Boolean(process.env.WEZTERM_PANE)`：变量存在但为空串时按非 WezTerm 处理。
#[test]
fn empty_wezterm_pane_is_not_treated_as_wezterm() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let saved = env_snapshot(&["WEZTERM_PANE", "TERM_PROGRAM"]);
    unsafe { std::env::set_var("WEZTERM_PANE", "") };
    unsafe { std::env::remove_var("TERM_PROGRAM") };
    set_images(kiana_tui::terminal_image::ImageProtocol::Kitty);
    let (mut tui, output) = alt(20, 4);
    let image_id = 10318u64;
    let image_line = kitty_line("AAAA", 3.0, image_id);
    kiana_tui::terminal_image::register_kitty_image_metadata(
        kiana_tui::terminal_image::KittyImageMetadata {
            image_id,
            columns: 2,
            rows: 3,
            width_px: 100.0,
            height_px: 100.0,
        },
    );
    let lines = Rc::new(RefCell::new(vec![
        image_line,
        String::new(),
        String::new(),
        "after".to_string(),
    ]));
    tui.set_layout_root_component(Some(Box::new(DynContent(Rc::clone(&lines)))));
    tui.start();
    tui.render_now(false);
    let before = output.borrow().len();

    lines.borrow_mut()[1] = "changed".to_string();
    tui.request_render(false);
    tui.render_now(false);
    let writes = output.borrow()[before..].to_string();
    // 非 WezTerm：只重写变化的行（`\x1b[2;1H\x1b[2Kchanged`），不做图片重绘。
    // WezTerm 三遍绘制会先删除全部 placement，并把每个变化行（含图片行）重新写出。
    assert!(
        writes.contains("\x1b[2;1H\x1b[2Kchanged"),
        "writes: {writes:?}"
    );
    assert!(
        !writes.contains("\x1b_Ga=d,d=a,q=2"),
        "empty WEZTERM_PANE must not delete/re-emit kitty placements: {writes:?}"
    );
    assert!(
        !writes.contains("\x1b_Ga=p,q=2"),
        "empty WEZTERM_PANE must not re-emit kitty placements: {writes:?}"
    );
    stop(&mut tui);
    kiana_tui::terminal_image::reset_capabilities_cache();
    env_restore(saved);
}

/// overlay 的 `render:false` 抑制重绘、`capture:true` 让后续 drag 直接重放到该 overlay
/// （即使指针已离开它的边界）。
#[test]
fn overlay_render_false_suppresses_frames_and_capture_routes_drags() {
    struct MouseOverlay {
        events: Rc<RefCell<Vec<(TuiMouseEventType, isize, isize)>>>,
    }

    impl Component for MouseOverlay {
        fn render(&mut self, _width: usize) -> Vec<String> {
            vec!["overlay".to_string()]
        }
        fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
            self.events
                .borrow_mut()
                .push((event.event_type, event.x, event.y));
            Some(TuiMouseEventResult {
                handled: Some(true),
                capture: Some(event.event_type == TuiMouseEventType::Press),
                render: Some(event.event_type != TuiMouseEventType::Press),
                ..Default::default()
            })
        }
    }

    let events = Rc::new(RefCell::new(Vec::new()));
    let (mut tui, _) = alt(20, 4);
    tui.add_child(Box::new(text("background")));
    tui.start();
    tui.render_now(false);
    let _handle = tui.show_overlay(
        Box::new(MouseOverlay {
            events: Rc::clone(&events),
        }),
        OverlayOptions {
            anchor: Some(OverlayAnchor::TopLeft),
            width: Some(SizeValue::Absolute(10.0)),
            max_height: Some(SizeValue::Absolute(2.0)),
            ..Default::default()
        },
    );
    tui.render_now(false);

    // press 在 overlay 内：handled + capture，但 render:false → 不得安排帧。
    tui.handle_terminal_input("\x1b[<0;2;1M");
    assert!(
        matches!(tui.render_decision(), kiana_tui::tui::RenderDecision::Idle),
        "render:false must suppress the frame"
    );

    // 指针移出 overlay 后，drag 仍直接投递给 capture 目标（局部坐标为负/越界）。
    tui.handle_terminal_input("\x1b[<32;18;4M");
    let events = events.borrow();
    assert_eq!(events.len(), 2, "events: {events:?}");
    assert_eq!(events[1].0, TuiMouseEventType::Drag);
    assert_eq!((events[1].1, events[1].2), (17, 3));
    drop(events);
    stop_after_overlay(&mut tui);
}

/// overlay handle 会占用栈；`stop` 之前显式隐藏（与 TS finally 里的 `tui.stop()` 等价）。
fn stop_after_overlay(tui: &mut TuiAltScreen) {
    tui.hide_overlay();
    stop(tui);
}

/// TS 的 `try { this.openUrl(...) } catch {}` / `try { this.onRightClickPaste() } catch {}`：
/// best-effort 回调抛错只影响该次操作，界面继续（Rust 侧 `catch_unwind` 吞掉 panic）。
#[test]
fn best_effort_callbacks_swallow_panics() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let saved = env_snapshot(&["TERM_PROGRAM"]);
    unsafe { std::env::remove_var("TERM_PROGRAM") };
    let (mut tui, _) = alt_with(
        20,
        3,
        TuiAltScreenOptions {
            open_url: Some(std::rc::Rc::new(|_url: &str| panic!("open_url boom"))),
            on_right_click_paste: Some(std::rc::Rc::new(|| panic!("right click paste boom"))),
            ..Default::default()
        },
    );
    tui.set_platform_override(Some("win32"));
    tui.add_child(Box::new(text(kiana_tui::terminal_image::hyperlink(
        "link",
        "https://example.com/",
    ))));
    tui.start();
    tui.render_now(false);

    // 左键按下 + 指定 release 码（OSC 8 激活路径）。
    tui.handle_terminal_input("\x1b[<0;2;1M");
    tui.handle_terminal_input("\x1b[<3;2;1m");
    // 右键按下（win32 的粘贴路径）。
    tui.handle_terminal_input("\x1b[<2;2;1M");
    stop(&mut tui);
    env_restore(saved);
}

// ---------------------------------------------------------------------------
// 鼠标 target 粒度回归（TS `dispatchMouseEvent` 返回**最内层**组件）
// ---------------------------------------------------------------------------

/// 记录鼠标事件（`name:type:x,y[:clickCount]`）并吞掉按压/拖拽/释放/点击的 MouseRegion。
fn recording_region(
    name: &'static str,
    label: &str,
    log: &Rc<RefCell<Vec<String>>>,
) -> Box<dyn Component> {
    let log = Rc::clone(log);
    Box::new(MouseRegion::new(
        Box::new(text(label)),
        Box::new(move |event: &TuiMouseEvent| {
            let (x, y) = (event.x, event.y);
            log.borrow_mut().push(match event.click_count {
                Some(count) => format!("{name}:{:?}:{x},{y},{count}", event.event_type),
                None => format!("{name}:{:?}:{x},{y}", event.event_type),
            });
            match event.event_type {
                TuiMouseEventType::Press
                | TuiMouseEventType::Drag
                | TuiMouseEventType::Release
                | TuiMouseEventType::Click => Some(TuiMouseEventResult {
                    handled: Some(true),
                    ..Default::default()
                }),
                _ => None,
            }
        }),
    ))
}

/// TS 的 `result.target.component` 是**最内层**命中的组件（`dispatchMouseEvent` 的
/// `"target" in result` 分支只透传子项的结果），因此按下后的拖拽/释放要直接投递给按下时的
/// 那个 MouseRegion，而不是重新经过外层 `Box` 按当前指针位置路由。
#[test]
fn replays_drag_and_release_to_the_innermost_nested_mouse_region() {
    let (mut tui, _) = alt(20, 4);
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut outer = BoxComponent::new(0, 0, None);
    outer.add_child(recording_region("A", "A", &log));
    outer.add_child(recording_region("B", "B", &log));
    tui.add_child(Box::new(outer));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;1;1M"); // press on A
    tui.handle_terminal_input("\x1b[<32;1;2M"); // drag to B's row
    tui.handle_terminal_input("\x1b[<0;1;2m"); // release on B's row
    tui.render_now(false);

    assert_eq!(
        *log.borrow(),
        vec![
            "A:Press:0,0".to_string(),
            "A:Drag:0,1".to_string(),
            "A:Release:0,1".to_string(),
        ]
    );
    stop(&mut tui);
}

/// 双击计数按**最内层**组件判定（TS `lastComponentClick.component`）：两次快速点击同一个
/// 嵌套 MouseRegion 得到 `clickCount` 1、2。
#[test]
fn counts_rapid_clicks_on_the_innermost_nested_mouse_region() {
    let (mut tui, _) = alt(20, 4);
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut outer = BoxComponent::new(0, 0, None);
    outer.add_child(recording_region("A", "A", &log));
    outer.add_child(recording_region("B", "B", &log));
    tui.add_child(Box::new(outer));
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;1;1M");
    tui.handle_terminal_input("\x1b[<0;1;1m");
    tui.handle_terminal_input("\x1b[<0;1;1M");
    tui.handle_terminal_input("\x1b[<0;1;1m");
    tui.render_now(false);

    assert_eq!(
        *log.borrow(),
        vec![
            "A:Press:0,0".to_string(),
            "A:Release:0,0".to_string(),
            "A:Click:0,0,1".to_string(),
            "A:Press:0,0".to_string(),
            "A:Release:0,0".to_string(),
            "A:Click:0,0,2".to_string(),
        ]
    );
    stop(&mut tui);
}

/// overlay 根是普通 `Container` 时，按下后的拖拽/释放同样投递给命中的 MouseRegion（指针移出
/// overlay 后仍直接重放，不再经过 overlay 容器的越界拦截）。
#[test]
fn replays_mouse_gestures_to_a_mouse_region_nested_in_an_overlay_container() {
    let (mut tui, _) = alt(20, 6);
    let log = Rc::new(RefCell::new(Vec::new()));
    tui.add_child(Box::new(text(
        "underlying1\nunderlying2\nunderlying3\nunderlying4\nunderlying5\nunderlying6",
    )));
    let mut container = Container::new();
    container.add_child(recording_region("R", "R", &log));
    tui.show_overlay(
        Box::new(container),
        OverlayOptions {
            anchor: Some(OverlayAnchor::TopLeft),
            width: Some(SizeValue::Absolute(10.0)),
            ..Default::default()
        },
    );
    tui.start();
    tui.render_now(false);

    tui.handle_terminal_input("\x1b[<0;1;1M"); // press inside the overlay
    tui.handle_terminal_input("\x1b[<32;1;3M"); // drag below the overlay
    tui.handle_terminal_input("\x1b[<0;1;3m"); // release below
    tui.render_now(false);

    assert_eq!(
        *log.borrow(),
        vec![
            "R:Press:0,0".to_string(),
            "R:Drag:0,2".to_string(),
            "R:Release:0,2".to_string(),
        ]
    );
    stop_after_overlay(&mut tui);
}
