//! `test/alt-screen-large-transcript-bench.ts` 的移植（`#[ignore]` 基准，非计时断言）。
//!
//! 形状与 coding agent 的全屏转录一致：
//! ScrollView → 文档 Container → header / resources / chat（2,500 个交替的 user/assistant
//! Markdown 组件）。Rust 侧用 [`Text`] 代替 Markdown（Markdown 属于 `components` 单元；
//! 本基准只测备用屏渲染管线的帧成本，不依赖具体组件的排版细节），并在无输出终端上跑
//! 稳态/滚行/流式/宽度变化四种场景。
//!
//! 运行：`cargo test -p kiana-tui --test alt_screen_large_transcript_bench -- --ignored --nocapture`

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
use kiana_tui::tui::{Component, Tui};

const COLUMNS: usize = 80;
const ROWS: usize = 50;
const COMPONENT_COUNT: usize = 2_500;
const WARMUP_FRAMES: usize = 20;

/// 丢弃输出的终端（`NullTerminal`）。
struct NullTerminal {
    columns: Rc<Cell<usize>>,
    rows: Rc<Cell<usize>>,
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
    fn write(&mut self, _data: &str) {}
    fn columns(&self) -> usize {
        self.columns.get()
    }
    fn rows(&self) -> usize {
        self.rows.get()
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

/// 把共享组件挂进栈（测试本地代理）。
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
    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        None
    }
    fn layout_node(&self) -> Option<kiana_tui::layout_node::LayoutNode> {
        self.0.borrow().layout_node()
    }
}

fn user_markdown() -> String {
    [
        "## User request",
        "",
        "Please inspect this implementation carefully and report the relevant behavior.",
        "",
        "- preserve correctness",
        "- include concrete evidence",
    ]
    .join("\n")
}

fn assistant_markdown() -> String {
    [
        "## Analysis",
        "",
        "This response contains representative prose with inline styles, a link, and enough words to wrap across terminal rows at realistic widths. The implementation should preserve all observable behavior while avoiding unnecessary allocation.",
        "",
        "1. First item with explanatory text and a concrete example.",
        "2. Second item with more explanatory text and another example.",
        "3. Third item that ensures list rendering is represented.",
        "",
        "```ts",
        "function render(width: number): string[] {",
        "  const lines: string[] = [];",
        "  for (const child of children) {",
        "    const childLines = child.render(width);",
        "    for (const line of childLines) lines.push(line);",
        "  }",
        "  return lines;",
        "}",
        "```",
        "",
        "> A quoted conclusion with enough text to exercise wrapping and inline rendering in the real Markdown component.",
        "",
        "Final paragraph with a concise recommendation and a note about correctness tests.",
    ]
    .join("\n")
}

#[test]
#[ignore = "benchmark; run with --ignored --nocapture"]
fn alt_screen_large_transcript_benchmark() {
    let chat = Rc::new(RefCell::new(kiana_tui::tui::Container::new()));
    let mut streaming: Option<Rc<RefCell<Text>>> = None;
    for index in 0..COMPONENT_COUNT {
        let content = if index % 2 == 0 {
            user_markdown()
        } else {
            assistant_markdown()
        };
        let component = Text::with_padding(content, 1, 0);
        if index + 1 == COMPONENT_COUNT {
            let shared = Rc::new(RefCell::new(component));
            streaming = Some(Rc::clone(&shared));
            chat.borrow_mut()
                .add_child(Box::new(SharedRef(shared as SharedComponent)));
        } else {
            chat.borrow_mut().add_child(Box::new(component));
        }
    }
    let streaming = streaming.expect("benchmark requires at least one assistant message");
    let header = Rc::new(RefCell::new(kiana_tui::tui::Container::new()));
    header.borrow_mut().add_child(Box::new(Text::with_padding(
        "kiana benchmark\nheader line",
        0,
        0,
    )));
    let resources = Rc::new(RefCell::new(kiana_tui::tui::Container::new()));
    resources
        .borrow_mut()
        .add_child(Box::new(Text::with_padding("resource", 0, 0)));
    let document = Rc::new(RefCell::new(kiana_tui::tui::Container::new()));
    document
        .borrow_mut()
        .add_child(Box::new(SharedRef(header.clone() as SharedComponent)));
    document
        .borrow_mut()
        .add_child(Box::new(SharedRef(resources.clone() as SharedComponent)));
    document
        .borrow_mut()
        .add_child(Box::new(SharedRef(chat.clone() as SharedComponent)));

    let transcript = Rc::new(RefCell::new(
        ScrollView::new(
            Box::new(SharedRef(document.clone() as SharedComponent)),
            ScrollViewOptions {
                follow: Some(ScrollViewFollow::None),
                primary: Some(true),
                ..Default::default()
            },
        )
        .unwrap(),
    ));
    let dock = VStack::new(
        vec![StackChild::Component(Box::new(Text::with_padding(
            "editor\nfooter",
            0,
            0,
        )))],
        StackOptions::default(),
    );
    let root = VStack::new(
        vec![
            StackChild::Entry(kiana_tui::components::stack::StackEntry::with_options(
                Box::new(SharedRef(transcript.clone() as SharedComponent)),
                StackEntryOptions {
                    basis: Some(0.0),
                    grow: Some(1.0),
                    min_size: Some(1.0),
                    ..Default::default()
                },
            )),
            StackChild::Component(Box::new(dock)),
        ],
        StackOptions::default(),
    );

    let columns = Rc::new(Cell::new(COLUMNS));
    let rows = Rc::new(Cell::new(ROWS));
    let terminal = NullTerminal {
        columns: Rc::clone(&columns),
        rows: Rc::clone(&rows),
    };
    let mut tui = TuiAltScreen::new(
        Box::new(terminal),
        Some(false),
        Some("/tmp/kiana-tui-bench".to_string()),
        TuiAltScreenOptions::default(),
    );
    tui.set_layout_root_component(Some(Box::new(root)));
    tui.start();
    tui.render_now(false);

    let total_lines = document.borrow_mut().render(COLUMNS).len();
    transcript
        .borrow()
        .scroll_to((total_lines / 2) as f64, Default::default());
    for _ in 0..WARMUP_FRAMES {
        tui.render_now(false);
    }

    let mut measure = |name: &str, frames: usize, before_frame: &mut dyn FnMut(usize)| {
        let start = Instant::now();
        for frame in 0..frames {
            before_frame(frame);
            tui.render_now(false);
        }
        let per_frame = start.elapsed().as_secs_f64() * 1000.0 / frames as f64;
        println!("{name}: {per_frame:.3} ms/frame ({frames} frames)");
    };

    println!("components={COMPONENT_COUNT} lines={total_lines} viewport={COLUMNS}x{ROWS}");
    measure("steady", 20, &mut |_| {});
    transcript
        .borrow()
        .scroll_to((total_lines / 2) as f64, Default::default());
    measure("scroll", 20, &mut |_| {
        transcript.borrow().scroll_by(1.0);
    });
    measure("streaming", 10, &mut |frame| {
        streaming.borrow_mut().set_text(format!(
            "{}\n\nstream {}",
            assistant_markdown(),
            "x".repeat(frame + 1)
        ));
    });
    measure("resize", 10, &mut |frame| {
        columns.set(if frame % 2 == 0 { COLUMNS - 1 } else { COLUMNS });
    });

    assert!(!tui.get_screen_lines().is_empty());
    tui.stop(kiana_tui::tui::TuiStopOptions::default());
}
