//! `test/tui-cell-size-input.test.ts` 的移植（2 个 it）。
//!
//! TS 用 `TERM_PROGRAM=ghostty` 触发真实的 `getCapabilities().images` 探测。Rust 侧的能力
//! 缓存是 thread-local（见 `terminal_image`），测试在**本线程**用 [`set_capabilities`] 播种
//! 同样的"终端支持图片"结果，然后走真实链路：`TuiMainScreen::start()` →
//! `queryCellSize()` →（终端回复）→ `consumeCellSizeResponse()` → `setCellDimensions()` →
//! [`get_cell_dimensions`]。断言与 TS 相同：启动时的 cell-size 查询不影响裸 ESC 的转发；
//! `CSI 6;h;w t` 被消费、**全局单元格尺寸**更新为 10x20，且后续输入照常到达聚焦组件。

#[path = "common/mod.rs"]
mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::TestTerminal;
use kiana_tui::tui::{Component, FocusTarget, Tui};
use kiana_tui::{
    CellDimensions, ImageProtocol, TerminalCapabilities, TuiMainScreen, TuiStopOptions,
    get_cell_dimensions, reset_capabilities_cache, set_capabilities, set_cell_dimensions,
};

struct InputRecorder {
    inputs: Rc<RefCell<Vec<String>>>,
}

impl Component for InputRecorder {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec![String::new()]
    }

    fn handle_input(&mut self, data: &str) {
        self.inputs.borrow_mut().push(data.to_string());
    }
}

/// 组件收到的输入。
type Inputs = Rc<RefCell<Vec<String>>>;

/// 终端累计写入。
type Output = Rc<RefCell<String>>;

/// `withImageTerminal(...)`：把能力缓存播种为"支持 Kitty 图片"（等价于真实终端
/// `TERM_PROGRAM=ghostty` 的能力探测结果），测试据此稳定触发图片相关分支。
fn with_image_capabilities<T>(f: impl FnOnce() -> T) -> T {
    set_capabilities(TerminalCapabilities {
        images: Some(ImageProtocol::Kitty),
        true_color: true,
        hyperlinks: true,
    });
    let result = f();
    reset_capabilities_cache();
    result
}

fn fixture() -> (TuiMainScreen, Inputs, Output) {
    let terminal = TestTerminal::with_size(80, 24);
    let output = terminal.output_handle();
    let mut tui = TuiMainScreen::new(Box::new(terminal));
    let inputs: Inputs = Rc::new(RefCell::new(Vec::new()));
    tui.add_child(Box::new(InputRecorder {
        inputs: Rc::clone(&inputs),
    }));
    tui.set_focus(Some(FocusTarget::Component { index: 0 }));
    (tui, inputs, output)
}

/// `forwards bare escape even when a cell size query was sent at startup`
#[test]
fn forwards_bare_escape_even_when_a_cell_size_query_was_sent_at_startup() {
    with_image_capabilities(|| {
        let (mut tui, inputs, output) = fixture();
        tui.start();
        // 启动确实发了 cell-size 查询（TS 里这是 `getCapabilities().images` 为真的结果）。
        assert!(output.borrow().contains("\x1b[16t"));

        tui.handle_input("\x1b");
        assert_eq!(*inputs.borrow(), vec!["\x1b".to_string()]);
        tui.stop(TuiStopOptions::default());
    });
}

/// `consumes cell size responses and still forwards later user input`
#[test]
fn consumes_cell_size_responses_and_still_forwards_later_user_input() {
    with_image_capabilities(|| {
        set_cell_dimensions(CellDimensions {
            width_px: 9.0,
            height_px: 18.0,
        });

        let (mut tui, inputs, _output) = fixture();
        tui.start();

        tui.handle_input("\x1b[6;20;10t");
        assert!(inputs.borrow().is_empty());
        // TS `getCellDimensions()` → `{ widthPx: 10, heightPx: 20 }`（真实全局状态，
        // 不是注入的 handler 记录）。
        let dimensions = get_cell_dimensions();
        assert_eq!(dimensions.width_px, 10.0);
        assert_eq!(dimensions.height_px, 20.0);

        tui.handle_input("q");
        assert_eq!(*inputs.borrow(), vec!["q".to_string()]);
        tui.stop(TuiStopOptions::default());
    });
}
