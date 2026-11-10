//! `packages/tui/test/tui-shrink.test.ts` 的移植（1 个 it）。
//!
//! TS 用 xterm 仿真屏断言"内容缩到 0 后，之前的三行从屏幕上消失"。Rust 侧不做终端仿真：
//! 主屏渲染器在"变化全在已删除行"分支只输出清行序列（`\r\x1b[2K` … `\x1b[{n}A`），
//! 因此断言**清空之后的写入流里不再出现旧内容**，与 TS 的 `viewport.some(includes(...))`
//! 等价（旧的文本只能来自这次写入，屏幕上不会凭空出现）。

use std::cell::RefCell;
use std::rc::Rc;

use kiana_tui::main_screen::TuiMainScreen;
use kiana_tui::terminal_colors::{ProgramStatus, Terminal};
use kiana_tui::tui::{Component, Tui, TuiStopOptions};

/// 固定行内容的组件（TS `Lines`）。
struct Lines {
    lines: Rc<RefCell<Vec<String>>>,
}

impl Component for Lines {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.lines.borrow().clone()
    }

    fn handles_input(&self) -> bool {
        false
    }
}

/// 记录写入的终端（不做屏幕仿真）。
struct RecordingTerminal {
    output: Rc<RefCell<String>>,
    columns: usize,
    rows: usize,
}

impl Terminal for RecordingTerminal {
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
        self.output.borrow_mut().push_str(data);
    }
    fn columns(&self) -> usize {
        self.columns
    }
    fn rows(&self) -> usize {
        self.rows
    }
    fn kitty_protocol_active(&self) -> bool {
        true
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

/// `clears all rendered lines when content shrinks to zero`
#[test]
fn clears_all_rendered_lines_when_content_shrinks_to_zero() {
    let output = Rc::new(RefCell::new(String::new()));
    let terminal = RecordingTerminal {
        output: Rc::clone(&output),
        columns: 40,
        rows: 10,
    };
    let mut tui = TuiMainScreen::new(Box::new(terminal));
    let content = Lines {
        lines: Rc::new(RefCell::new(vec![
            "first".to_string(),
            "second".to_string(),
            "third".to_string(),
        ])),
    };
    tui.add_child(Box::new(content));
    tui.start();
    tui.render_now(false);

    let first_render = output.borrow().clone();
    assert!(first_render.contains("first"));
    assert!(first_render.contains("second"));
    assert!(first_render.contains("third"));

    output.borrow_mut().clear();
    tui.clear_children();
    tui.request_render(false);
    tui.render_now(false);

    let after_shrink = output.borrow().clone();
    assert!(
        !after_shrink.contains("first"),
        "first line should be cleared"
    );
    assert!(
        !after_shrink.contains("second"),
        "second line should be cleared"
    );
    assert!(
        !after_shrink.contains("third"),
        "third line should be cleared"
    );
    // 清行路径：每行一条 `\r\x1b[2K`，最后整体上移。
    assert_eq!(after_shrink.matches("\r\x1b[2K").count(), 3);
    tui.stop(TuiStopOptions::default());
}
