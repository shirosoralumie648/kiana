//! `test/render-churn-bench.ts` 的移植（`#[ignore]` 基准）。
//!
//! 两个场景：static（帧间无变化，纯重组装开销）与 editor（每帧向编辑器追加一个字符）。
//! TS 用 V8 采样堆分析器估算分配量；Rust 侧没有等价物，因此只打印每帧耗时与写出字节数
//! （分配量属于实现语言差异，不在移植目标内）。
//!
//! 运行：`cargo test -p kiana-tui --test render_churn_bench -- --ignored --nocapture`

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use kiana_tui::alt_screen::{TuiAltScreen, TuiAltScreenOptions};
use kiana_tui::components::scroll_view::ScrollViewScrollbar;
use kiana_tui::components::scroll_view::{ScrollView, ScrollViewFollow, ScrollViewOptions};
use kiana_tui::components::stack::{StackChild, StackEntryOptions, StackOptions};
use kiana_tui::components::text_component::Text;
use kiana_tui::components::v_stack::VStack;
use kiana_tui::layout_node::SharedComponent;
use kiana_tui::terminal_colors::{ProgramStatus, Terminal};
use kiana_tui::tui::{CURSOR_MARKER, Component, Tui};

const COLUMNS: usize = 100;
const ROWS: usize = 30;
const WARMUP_FRAMES: usize = 20;
const FRAMES: usize = 60;

/// 丢弃输出但统计字节数的终端。
struct NullTerminal {
    columns: usize,
    rows: usize,
    bytes_written: Rc<Cell<usize>>,
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
        self.bytes_written
            .set(self.bytes_written.get() + data.len());
    }
    fn columns(&self) -> usize {
        self.columns
    }
    fn rows(&self) -> usize {
        self.rows
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

/// 共享组件代理（测试本地）。
struct SharedRef(SharedComponent);

impl Component for SharedRef {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }
    fn handle_mouse(
        &mut self,
        event: &kiana_tui::tui::TuiMouseEvent,
    ) -> Option<kiana_tui::tui::TuiMouseEventResult> {
        self.0.borrow_mut().handle_mouse(event)
    }
    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }
    fn layout_node(&self) -> Option<kiana_tui::layout_node::LayoutNode> {
        self.0.borrow().layout_node()
    }
}

/// 编辑器替身：按 (text, width) 缓存行，文本变化时重渲染（TS `EditorSim`）。
struct EditorSim {
    text: String,
    cached_text: Option<String>,
    cached_width: Option<usize>,
    cached_lines: Option<Vec<String>>,
}

impl EditorSim {
    fn new() -> Self {
        EditorSim {
            text: String::new(),
            cached_text: None,
            cached_width: None,
            cached_lines: None,
        }
    }

    fn append(&mut self, ch: char) {
        self.text.push(ch);
    }
}

impl Component for EditorSim {
    fn invalidate(&mut self) {
        self.cached_text = None;
        self.cached_width = None;
        self.cached_lines = None;
    }

    fn render(&mut self, width: usize) -> Vec<String> {
        if let (Some(lines), Some(cached_text), Some(cached_width)) =
            (&self.cached_lines, &self.cached_text, self.cached_width)
            && *cached_text == self.text
            && cached_width == width
        {
            return lines.clone();
        }
        let border = format!("\x1b[90m{}\x1b[39m", "─".repeat(width.max(2) - 2));
        let lines = vec![
            border.clone(),
            format!(" > {}{CURSOR_MARKER}", self.text),
            border,
        ];
        self.cached_text = Some(self.text.clone());
        self.cached_width = Some(width);
        self.cached_lines = Some(lines.clone());
        lines
    }
}

fn build_transcript() -> Rc<RefCell<kiana_tui::tui::Container>> {
    let container = Rc::new(RefCell::new(kiana_tui::tui::Container::new()));
    for index in 0..150 {
        let styled = if index % 3 == 0 {
            format!(
                "\x1b[1m\x1b[36muser {index}\x1b[39m\x1b[22m message with some \x1b[33mstyled\x1b[39m content padding padding"
            )
        } else {
            format!(
                "assistant {index} plain response line with enough text to be representative of a transcript row"
            )
        };
        container
            .borrow_mut()
            .add_child(Box::new(Text::with_padding(styled, 1, 0)));
    }
    container
}

fn run_scenario(
    tui: &mut TuiAltScreen,
    bytes: &Rc<Cell<usize>>,
    mut frame: impl FnMut(usize),
) -> (f64, usize) {
    let before = bytes.get();
    let start = Instant::now();
    for index in 0..FRAMES {
        frame(index);
        tui.render_now(false);
    }
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    (elapsed / FRAMES as f64, bytes.get() - before)
}

#[test]
#[ignore = "benchmark; run with --ignored --nocapture"]
fn render_churn_benchmark() {
    let bytes = Rc::new(Cell::new(0usize));
    let terminal = NullTerminal {
        columns: COLUMNS,
        rows: ROWS,
        bytes_written: Rc::clone(&bytes),
    };
    let mut tui = TuiAltScreen::new(
        Box::new(terminal),
        Some(false),
        Some("/tmp/kiana-tui-bench".to_string()),
        TuiAltScreenOptions::default(),
    );

    let transcript = build_transcript();
    let editor = Rc::new(RefCell::new(EditorSim::new()));
    let scroll_view = Rc::new(RefCell::new(
        ScrollView::new(
            Box::new(SharedRef(transcript.clone() as SharedComponent)),
            ScrollViewOptions {
                follow: Some(ScrollViewFollow::End),
                primary: Some(true),
                overscroll: Some(kiana_tui::layout_node::Overscroll::Chain),
                scrollbar: Some(ScrollViewScrollbar::Auto),
                ..Default::default()
            },
        )
        .unwrap(),
    ));
    let status = Text::with_padding("\x1b[2mstatus: idle\x1b[22m", 1, 0);
    let footer = Text::with_padding("\x1b[2m~/workspaces/kiana  main  100k tokens\x1b[22m", 1, 0);
    let dock = VStack::new(
        vec![
            StackChild::Component(Box::new(status)),
            StackChild::Entry(kiana_tui::components::stack::StackEntry::with_options(
                Box::new(SharedRef(editor.clone() as SharedComponent)),
                StackEntryOptions {
                    shrink: Some(1.0),
                    min_size: Some(3.0),
                    ..Default::default()
                },
            )),
            StackChild::Component(Box::new(footer)),
        ],
        StackOptions::default(),
    );
    let root = VStack::new(
        vec![
            StackChild::Entry(kiana_tui::components::stack::StackEntry::with_options(
                Box::new(SharedRef(scroll_view.clone() as SharedComponent)),
                StackEntryOptions {
                    basis: Some(0.0),
                    grow: Some(1.0),
                    shrink: Some(1.0),
                    min_size: Some(1.0),
                    ..Default::default()
                },
            )),
            StackChild::Component(Box::new(dock)),
        ],
        StackOptions::default(),
    );
    tui.set_layout_root_component(Some(Box::new(root)));
    tui.start();
    for _ in 0..WARMUP_FRAMES {
        tui.render_now(false);
    }

    let (static_ms, static_bytes) = run_scenario(&mut tui, &bytes, |_| {});
    let (editor_ms, editor_bytes) = run_scenario(&mut tui, &bytes, |index| {
        editor
            .borrow_mut()
            .append(char::from(b'a' + (index % 26) as u8));
    });
    println!(
        "frames={FRAMES} viewport={COLUMNS}x{ROWS} transcript={} lines",
        transcript.borrow_mut().render(COLUMNS).len()
    );
    println!(
        "static   {:.3} ms/frame  {} written bytes/frame",
        static_ms,
        static_bytes / FRAMES
    );
    println!(
        "editor   {:.3} ms/frame  {} written bytes/frame",
        editor_ms,
        editor_bytes / FRAMES
    );
    assert!(static_bytes > 0);
    tui.stop(kiana_tui::tui::TuiStopOptions::default());
}
