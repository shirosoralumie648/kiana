//! 备用屏渲染基准（Criterion-free；`cargo bench --bench alt_screen_bench`）。
//!
//! 对应 `test/alt-screen-large-transcript-bench.ts` 的稳态场景精简版：在丢弃输出的终端上
//! 反复渲染一个 2000 行的转录视口，打印每帧耗时。完整场景（滚动/流式/宽度变化）见
//! `tests/alt_screen_large_transcript_bench.rs`。

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use kiana_tui::alt_screen::{TuiAltScreen, TuiAltScreenOptions};
use kiana_tui::components::scroll_view::{ScrollView, ScrollViewFollow, ScrollViewOptions};
use kiana_tui::components::text_component::Text;
use kiana_tui::terminal_colors::{ProgramStatus, Terminal};
use kiana_tui::tui::Tui;

struct NullTerminal;

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
        80
    }
    fn rows(&self) -> usize {
        50
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

#[test]
fn alt_screen_steady_frames() {
    let transcript = Rc::new(RefCell::new(
        ScrollView::new(
            Box::new(Text::with_padding(
                (0..2000)
                    .map(|index| format!("transcript line {index} with representative content"))
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
    let mut tui = TuiAltScreen::new(
        Box::new(NullTerminal),
        Some(false),
        None,
        TuiAltScreenOptions::default(),
    );
    tui.set_layout_root_component(Some(Box::new(TestRoot(transcript.clone()))));
    tui.start();
    for _ in 0..20 {
        tui.render_now(false);
    }
    let start = Instant::now();
    let frames = 200;
    for _ in 0..frames {
        tui.render_now(false);
    }
    println!(
        "steady: {:.3} ms/frame ({frames} frames)",
        start.elapsed().as_secs_f64() * 1000.0 / frames as f64
    );
    tui.stop(kiana_tui::tui::TuiStopOptions::default());
}

struct TestRoot(Rc<RefCell<ScrollView>>);

impl kiana_tui::tui::Component for TestRoot {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }
    fn layout_node(&self) -> Option<kiana_tui::layout_node::LayoutNode> {
        self.0.borrow().layout_node()
    }
}
