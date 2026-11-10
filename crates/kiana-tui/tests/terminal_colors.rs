//! `test/terminal-colors.test.ts`：终端配色上报与 OSC 10/11/4 回复的解析，以及
//! `TUI.queryTerminalColors` 的完整行为（解析 2 条 + 查询 4 条）。
//!
//! TS 用 `TuiMainScreen` 驱动；`queryTerminalColors` / `handleTerminalInput` 都定义在
//! `TuiBase` 上（主屏只是子类），因此这些用例直接驱动 `TuiBase` —— 不需要
//! `tui-main-screen` 单元。`sendInput` 在 TS 里最终调用的就是 `handleTerminalInput`。

#[path = "common/mod.rs"]
mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::TestTerminal;
use kiana_tui::terminal_colors::{
    OscColorTarget, RgbColor, TerminalColorScheme, TerminalColors, parse_osc_color_response,
    parse_terminal_color_scheme_report,
};
use kiana_tui::tui::{Component, FocusTarget, TERMINAL_COLOR_QUERY, TuiBase, TuiStopOptions};

const WHITE: RgbColor = RgbColor {
    r: 255,
    g: 255,
    b: 255,
};
const BLACK: RgbColor = RgbColor { r: 0, g: 0, b: 0 };
const DA1: &str = "\x1b[?62;22c";

#[test]
fn parses_color_scheme_reports() {
    assert_eq!(
        parse_terminal_color_scheme_report("\x1b[?997;1n"),
        Some(TerminalColorScheme::Dark)
    );
    assert_eq!(
        parse_terminal_color_scheme_report("\x1b[?997;2n"),
        Some(TerminalColorScheme::Light)
    );
    // 多个上报时以**最后**一个为准
    assert_eq!(
        parse_terminal_color_scheme_report("\x1b[?997;2n\x1b[?997;1n\x1b[?997;1n"),
        Some(TerminalColorScheme::Dark)
    );
    assert_eq!(
        parse_terminal_color_scheme_report("\x1b[?997;1n\x1b[?997;2n\x1b[?997;2n"),
        Some(TerminalColorScheme::Light)
    );
    assert_eq!(parse_terminal_color_scheme_report("\x1b[?997;3n"), None);
    assert_eq!(parse_terminal_color_scheme_report("\x1b[?996n"), None);
    assert_eq!(parse_terminal_color_scheme_report("x\x1b[?997;1n"), None);
}

#[test]
fn parses_osc_10_11_and_4_replies() {
    assert_eq!(
        parse_osc_color_response("\x1b]10;rgb:ffff/ffff/ffff\x07"),
        Some((OscColorTarget::Foreground, Some(WHITE)))
    );
    assert_eq!(
        parse_osc_color_response("\x1b]4;13;#ff0080\x1b\\"),
        Some((
            OscColorTarget::Palette(13),
            Some(RgbColor {
                r: 255,
                g: 0,
                b: 128
            })
        ))
    );
    // 无法解析的色值保留目标，rgb 为 None
    assert_eq!(
        parse_osc_color_response("\x1b]4;1;bogus\x07"),
        Some((OscColorTarget::Palette(1), None))
    );
    assert_eq!(parse_osc_color_response("\x1b]12;#ffffff\x07"), None);
}

// ---------------------------------------------------------------------------
// describe("TUI.queryTerminalColors")
// ---------------------------------------------------------------------------

/// 记录键盘输入的组件（TS `InputRecorder`）。
struct InputRecorder {
    inputs: Rc<RefCell<Vec<String>>>,
}

impl InputRecorder {
    fn new() -> Self {
        InputRecorder {
            inputs: Rc::new(RefCell::new(Vec::new())),
        }
    }

    fn handle(&self) -> Rc<RefCell<Vec<String>>> {
        Rc::clone(&self.inputs)
    }
}

impl Component for InputRecorder {
    fn render(&mut self, _width: usize) -> Vec<String> {
        Vec::new()
    }

    fn handle_input(&mut self, data: &str) {
        self.inputs.borrow_mut().push(data.to_string());
    }
}

/// TS `PALETTE_REPLIES`：`Array.from({ length: 16 }, (_, i) => \`\x1b]4;${i};#000000\x07\`)`。
fn palette_replies() -> Vec<String> {
    (0..16)
        .map(|index| format!("\x1b]4;{index};#000000\x07"))
        .collect()
}

struct Fixture {
    base: TuiBase,
    inputs: Rc<RefCell<Vec<String>>>,
    output: Rc<RefCell<String>>,
}

/// TS `setup()`：建 TUI、加一个 `InputRecorder` 子组件、聚焦它并 `start()`。
fn setup() -> Fixture {
    let terminal = TestTerminal::with_size(80, 24);
    let output = terminal.output_handle();
    let mut base = TuiBase::new(Box::new(terminal), None, None);
    let recorder = InputRecorder::new();
    let inputs = recorder.handle();
    base.add_child(Box::new(recorder));
    base.set_focus(Some(FocusTarget::Component { index: 0 }));
    base.start();
    Fixture {
        base,
        inputs,
        output,
    }
}

impl Fixture {
    /// TS `terminal.sendInput(data)`：`onInput` 回调最终走 `handleTerminalInput`。
    fn send_input(&mut self, data: &str) {
        self.base.handle_terminal_input(data);
    }

    fn stop(&mut self) {
        self.base.stop(TuiStopOptions::default());
    }
}

#[tokio::test]
async fn queries_all_colors_in_one_write_and_consumes_the_replies() {
    let mut fixture = setup();
    // 查询发出时同步写入 TERMINAL_COLOR_QUERY（TS 的 `terminal.write` 在 Promise 构造器里）。
    let query = fixture
        .base
        .query_terminal_colors_with_late_reply(1000, None);
    let written = fixture.output.borrow().clone();
    let written_query = written
        .rsplit_once("\x1b[?25l")
        .map(|(_, tail)| tail.to_string())
        .unwrap_or(written);
    assert!(
        written_query.contains(TERMINAL_COLOR_QUERY),
        "query not written"
    );
    assert!(written_query.starts_with("\x1b]10;?\x07\x1b]11;?\x07\x1b]4;0;?\x07"));
    assert!(written_query.ends_with("\x1b[c"));

    fixture.send_input("x");
    fixture.send_input("\x1b]10;#ffffff\x07");
    fixture.send_input("\x1b]11;rgb:0000/0000/0000\x1b\\");
    for reply in palette_replies() {
        fixture.send_input(&reply);
    }
    // 收齐全部回复即 resolve，无需等 DA1。
    let colors = query.await;
    assert_eq!(
        colors,
        TerminalColors {
            foreground: Some(WHITE),
            background: Some(BLACK),
            palette: Some(vec![BLACK; 16]),
        }
    );
    fixture.send_input(DA1);
    assert_eq!(*fixture.inputs.borrow(), vec!["x".to_string()]);
    fixture.stop();
}

/// TS 测试里的 `StdinBuffer` 等价物：只在收到 OSC/CSI 终止符（BEL 或 `ESC \\`）后才把序列
/// 交给下游，因此半截回复不会变成"用户输入"。
///
/// 真正的 `StdinBuffer` 属于 stdin 单元（`crates/kiana-tui/src/stdin.rs`）；这里只放足以驱动
/// 本条用例的最小逻辑，避免依赖兄弟单元。
#[derive(Default)]
struct StdinBufferLike {
    pending: String,
}

impl StdinBufferLike {
    /// `buffer.process(data)`：切出完整序列并返回它们。
    fn process(&mut self, data: &str) -> Vec<String> {
        self.pending.push_str(data);
        let mut out = Vec::new();
        loop {
            let bytes = self.pending.as_bytes();
            let Some(start) = self.pending.find("\x1b") else {
                // 没有转义序列：普通文本直接透传。
                if !self.pending.is_empty() {
                    out.push(std::mem::take(&mut self.pending));
                }
                break;
            };
            let mut end = None;
            if bytes.get(start + 1) == Some(&b'[') {
                // CSI：终止字节在 0x40..=0x7E。
                let mut index = start + 2;
                while index < bytes.len() {
                    if (0x40..=0x7E).contains(&bytes[index]) {
                        end = Some(index + 1);
                        break;
                    }
                    index += 1;
                }
            }
            let mut index = start + 1;
            if end.is_none() {
                while index < bytes.len() {
                    if bytes[index] == 0x07 {
                        end = Some(index + 1);
                        break;
                    }
                    if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'\\') {
                        end = Some(index + 2);
                        break;
                    }
                    index += 1;
                }
            }
            let Some(end) = end else {
                // 不完整：头部已完整的部分先透传，剩下的留在缓冲区。
                if start > 0 {
                    out.push(self.pending[..start].to_string());
                    self.pending = self.pending[start..].to_string();
                }
                break;
            };
            out.push(self.pending[..end].to_string());
            self.pending = self.pending[end..].to_string();
        }
        out
    }
}

#[tokio::test]
async fn consumes_delayed_ghostty_color_fragments_without_typing_their_tail_or_opening_the_editor()
{
    let mut fixture = setup();
    let query = fixture
        .base
        .query_terminal_colors_with_late_reply(1000, None);
    let mut buffer = StdinBufferLike::default();

    // Ghostty 1.3.1 + Ayu Light 的分片回复（在报告的尾部切开）。
    for chunk in buffer.process("\x1b]4;1;rgb:eaea/6c") {
        fixture.send_input(&chunk);
    }
    for chunk in buffer.process(&format!("6c/6d6d\x07{DA1}")) {
        fixture.send_input(&chunk);
    }
    assert_eq!(query.await, TerminalColors::default());
    assert!(fixture.inputs.borrow().is_empty());

    // 回复之后真正的 Ctrl+G 仍然到达编辑器。
    for chunk in buffer.process("\x07") {
        fixture.send_input(&chunk);
    }
    assert_eq!(*fixture.inputs.borrow(), vec!["\x07".to_string()]);
    fixture.stop();
}

#[tokio::test]
async fn resolves_on_da1_with_the_replies_that_arrived_in_query_order() {
    let mut fixture = setup();
    let first = fixture
        .base
        .query_terminal_colors_with_late_reply(1000, None);
    let second = fixture
        .base
        .query_terminal_colors_with_late_reply(1000, None);

    fixture.send_input("\x1b]11;#000000\x07");
    // 不完整的调色板会被丢弃。
    for reply in palette_replies().into_iter().take(8) {
        fixture.send_input(&reply);
    }
    fixture.send_input(DA1);
    fixture.send_input(DA1);

    assert_eq!(
        first.await,
        TerminalColors {
            foreground: None,
            background: Some(BLACK),
            palette: None,
        }
    );
    assert_eq!(second.await, TerminalColors::default());
    fixture.stop();
}

#[tokio::test]
async fn reports_late_replies_after_a_timeout_and_consumes_them_until_da1() {
    let mut fixture = setup();
    let late: Rc<RefCell<Vec<TerminalColors>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&late);
    let result = fixture
        .base
        .query_terminal_colors_with_late_reply(
            1,
            Some(Box::new(move |colors| sink.borrow_mut().push(colors))),
        )
        .await;
    assert_eq!(result.background, None);

    fixture.send_input("\x1b]11;#ffffff\x07");
    fixture.send_input(DA1);
    assert_eq!(
        *late.borrow(),
        vec![TerminalColors {
            foreground: None,
            background: Some(WHITE),
            palette: None,
        }]
    );
    assert!(fixture.inputs.borrow().is_empty());

    // 没有未决查询时，颜色回复重新变成普通输入。
    fixture.send_input("\x1b]11;#ffffff\x07");
    assert_eq!(
        *fixture.inputs.borrow(),
        vec!["\x1b]11;#ffffff\x07".to_string()]
    );
    fixture.stop();
}

// ---------------------------------------------------------------------------
// 回归：OSC 4 回复有两个分号
// ---------------------------------------------------------------------------

/// `ESC ]4;<index>;<value>` 比 `ESC ]10/11;<value>` 多一个分号；按第一个分号切分会把
/// 索引当成 `4`、把 `<index>;<value>` 当成色值，于是 `rgb` 恒为 `None`。
#[test]
fn parses_osc_4_replies_that_carry_two_separators() {
    assert_eq!(
        parse_osc_color_response("\x1b]4;13;#ff0080\x07"),
        Some((
            OscColorTarget::Palette(13),
            Some(RgbColor {
                r: 255,
                g: 0,
                b: 128
            })
        ))
    );
    assert_eq!(
        parse_osc_color_response("\x1b]4;0;rgb:0000/0000/0000\x1b\\"),
        Some((
            OscColorTarget::Palette(0),
            Some(RgbColor { r: 0, g: 0, b: 0 })
        ))
    );
    // 索引非法 / 缺值时不认
    assert_eq!(parse_osc_color_response("\x1b]4;;#fff\x07"), None);
    assert_eq!(parse_osc_color_response("\x1b]4;1000;#fff\x07"), None);
    assert_eq!(parse_osc_color_response("\x1b]4;13\x07"), None);
}
