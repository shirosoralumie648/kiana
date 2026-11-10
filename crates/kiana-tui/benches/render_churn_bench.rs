//! 渲染 churn 基准（Criterion-free；`cargo bench --bench render_churn_bench`）。
//!
//! 对应 `test/render-churn-bench.ts` 的 editor 场景精简版：每帧向编辑器追加一个字符，
//! 在丢弃输出的终端上打印每帧耗时与写出字节数。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use kiana_tui::alt_screen::{TuiAltScreen, TuiAltScreenOptions};
use kiana_tui::components::scroll_view::{ScrollView, ScrollViewFollow, ScrollViewOptions};
use kiana_tui::components::stack::{StackChild, StackEntryOptions, StackOptions};
use kiana_tui::components::text_component::Text;
use kiana_tui::components::v_stack::VStack;
use kiana_tui::layout_node::SharedComponent;
use kiana_tui::terminal_colors::{ProgramStatus, Terminal};
use kiana_tui::tui::{CURSOR_MARKER, Component, Tui};

struct NullTerminal {
    bytes: Rc<Cell<usize>>,
}

impl Terminal for NullTerminal {
    fn start(&mut self, _on_input: Box<dyn FnMut(&str)>, _on_resize: Box<dyn FnMut()>) {}
    fn stop(&mut self) {}
    fn drain_input(
        &mut self,
        _max_ms: u64,
        _idle_ms: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + '_>> {
        Box::pin(async {})
    }
    fn write(&mut self, data: &str) {
        self.bytes.set(self.bytes.get() + data.len());
    }
    fn columns(&self) -> usize {
        100
    }
    fn rows(&self) -> usize {
        30
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

struct SharedRef(SharedComponent);

impl Component for SharedRef {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }
    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }
    fn layout_node(&self) -> Option<kiana_tui::layout_node::LayoutNode> {
        self.0.borrow().layout_node()
    }
}

#[derive(Default)]
struct EditorSim {
    text: String,
}

impl Component for EditorSim {
    fn render(&mut self, width: usize) -> Vec<String> {
        let border = format!("\x1b[90m{}\x1b[39m", "─".repeat(width.max(2) - 2));
        vec![
            border.clone(),
            format!(" > {}{CURSOR_MARKER}", self.text),
            border,
        ]
    }
}

#[test]
fn render_churn_editor_frames() {
    let bytes = Rc::new(Cell::new(0usize));
    let transcript = Rc::new(RefCell::new(
        ScrollView::new(
            Box::new(Text::with_padding(
                (0..150)
                    .map(|index| {
                        format!("assistant {index} plain response line with representative content")
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                1,
                0,
            )),
            ScrollViewOptions {
                follow: Some(ScrollViewFollow::End),
                primary: Some(true),
                ..Default::default()
            },
        )
        .unwrap(),
    ));
    let editor = Rc::new(RefCell::new(EditorSim::default()));
    let root = VStack::new(
        vec![
            StackChild::Entry(kiana_tui::components::stack::StackEntry::with_options(
                Box::new(SharedRef(transcript as SharedComponent)),
                StackEntryOptions {
                    basis: Some(0.0),
                    grow: Some(1.0),
                    min_size: Some(1.0),
                    ..Default::default()
                },
            )),
            StackChild::Entry(kiana_tui::components::stack::StackEntry::with_options(
                Box::new(SharedRef(editor.clone() as SharedComponent)),
                StackEntryOptions {
                    min_size: Some(3.0),
                    ..Default::default()
                },
            )),
        ],
        StackOptions::default(),
    );
    let mut tui = TuiAltScreen::new(
        Box::new(NullTerminal {
            bytes: Rc::clone(&bytes),
        }),
        Some(false),
        None,
        TuiAltScreenOptions::default(),
    );
    tui.set_layout_root_component(Some(Box::new(root)));
    tui.start();
    for _ in 0..20 {
        tui.render_now(false);
    }
    let before = bytes.get();
    let frames = 200;
    let start = Instant::now();
    for index in 0..frames {
        editor
            .borrow_mut()
            .text
            .push(char::from(b'a' + (index % 26) as u8));
        editor.borrow_mut().invalidate();
        tui.render_now(false);
    }
    println!(
        "editor: {:.3} ms/frame  {} written bytes/frame ({frames} frames)",
        start.elapsed().as_secs_f64() * 1000.0 / frames as f64,
        (bytes.get() - before) / frames
    );
    tui.stop(kiana_tui::tui::TuiStopOptions::default());
}
