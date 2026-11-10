//! 第 12 周帧契约测试：ack 去重、行级 diff、overlay、光标、备用屏 golden、
//! 超宽行与 10k 行 transcript 的 resize 一致性。

use kiana_interactive::render_core::frame::{
    BEGIN_SYNCHRONIZED_OUTPUT, CLEAR_SCREEN_AND_SCROLLBACK, END_SYNCHRONIZED_OUTPUT,
    ENTER_ALT_SCREEN, EXIT_ALT_SCREEN, FailingSink, FrameProvider, FrameWriteError, FrameWriter,
    FrameWriterOptions, HistoryBatch, Multiplexer, OverlayFrame, StringSink, TerminalFramePlan,
    TerminalMode, ViewportSize,
};
use kiana_interactive::render_core::history::{TranscriptFrameProvider, TranscriptLedger};
use kiana_tui::text::visible_width;

struct StaticProvider {
    history: Option<HistoryBatch>,
    viewport: Vec<String>,
}

impl FrameProvider for StaticProvider {
    fn render_frame(&mut self, _viewport: ViewportSize) -> TerminalFramePlan {
        TerminalFramePlan {
            history: self.history.clone(),
            viewport: self.viewport.clone(),
        }
    }
}

/// 只有收到 ack 才释放 batch 的 provider；`ignore_ack` 用来模拟 ack 丢失。
struct PendingProvider {
    batch: HistoryBatch,
    viewport: Vec<String>,
    ignore_ack: bool,
    acks: Vec<u64>,
}

impl FrameProvider for PendingProvider {
    fn render_frame(&mut self, _viewport: ViewportSize) -> TerminalFramePlan {
        TerminalFramePlan {
            history: Some(self.batch.clone()),
            viewport: self.viewport.clone(),
        }
    }
    fn acknowledge_history(&mut self, id: u64) {
        self.acks.push(id);
        if !self.ignore_ack && id == self.batch.id {
            self.batch.id = 0; // 0 表示已释放；provider 不再提供。
        }
    }
}

fn size_80x24() -> ViewportSize {
    ViewportSize {
        columns: 80,
        rows: 24,
    }
}

#[test]
fn single_char_change_is_one_move_and_one_rewrite() {
    let mut provider = StaticProvider {
        history: None,
        viewport: vec!["hello".to_string(), "world".to_string()],
    };
    let mut writer = FrameWriter::new(FrameWriterOptions::default());
    let mut sink = StringSink::default();
    writer
        .render(&mut provider, size_80x24(), &mut sink)
        .unwrap();

    provider.viewport = vec!["hallo".to_string(), "world".to_string()];
    let output = writer
        .render(&mut provider, size_80x24(), &mut sink)
        .unwrap();
    assert_eq!(output.write_calls, 1, "一帧只调用一次终端 write");
    assert_eq!(output.content_moves, 1, "只有一行变化");
    assert_eq!(output.line_rewrites, 1);
    assert!(!output.bytes.contains("hello"));
    assert!(output.bytes.contains("hallo"));
}

#[test]
fn dropped_ack_batch_is_not_written_twice() {
    let mut provider = PendingProvider {
        batch: HistoryBatch::append(7, vec!["committed-row".to_string()]),
        viewport: vec!["tail".to_string()],
        ignore_ack: true,
        acks: Vec::new(),
    };
    let mut writer = FrameWriter::new(FrameWriterOptions::default());
    let mut sink = StringSink::default();

    let first = writer
        .render(&mut provider, size_80x24(), &mut sink)
        .unwrap();
    assert_eq!(first.history_rows_written, 1);
    assert_eq!(first.accepted_history_id, Some(7));

    let second = writer
        .render(&mut provider, size_80x24(), &mut sink)
        .unwrap();
    assert_eq!(second.history_rows_written, 0, "重复 batch 不能再写");
    assert_eq!(second.duplicate_acknowledgements, 1);
    assert_eq!(
        sink.output.matches("committed-row").count(),
        1,
        "scrollback 里只能有一份"
    );
    assert_eq!(writer.committed_rows(), ["committed-row"]);
}

#[test]
fn write_failure_does_not_ack_and_retry_succeeds() {
    let mut provider = PendingProvider {
        batch: HistoryBatch::append(1, vec!["only-once".to_string()]),
        viewport: vec!["viewport".to_string()],
        ignore_ack: false,
        acks: Vec::new(),
    };
    let mut writer = FrameWriter::new(FrameWriterOptions::default());
    let mut failing = FailingSink::default();
    let error = writer
        .render(&mut provider, size_80x24(), &mut failing)
        .unwrap_err();
    assert!(matches!(error, FrameWriteError::Io(_)));
    assert_eq!(writer.accepted_history_id(), 0);
    assert!(provider.acks.is_empty(), "写失败不能 ack");
    assert!(writer.committed_rows().is_empty());

    let mut sink = StringSink::default();
    let output = writer
        .render(&mut provider, size_80x24(), &mut sink)
        .unwrap();
    assert_eq!(output.accepted_history_id, Some(1));
    assert_eq!(provider.acks, vec![1]);
    assert_eq!(sink.output.matches("only-once").count(), 1);
}

#[test]
fn replay_is_one_atomic_write_with_clear() {
    let mut provider = StaticProvider {
        history: Some(HistoryBatch::replay(
            3,
            vec!["a".to_string(), "b".to_string()],
        )),
        viewport: vec!["c".to_string()],
    };
    let mut writer = FrameWriter::new(FrameWriterOptions::default());
    let mut sink = StringSink::default();
    let output = writer
        .render(&mut provider, size_80x24(), &mut sink)
        .unwrap();
    assert_eq!(output.write_calls, 1);
    assert!(output.bytes.contains(CLEAR_SCREEN_AND_SCROLLBACK));
    assert_eq!(output.history_rows_written, 2);
    assert_eq!(writer.committed_rows(), ["a", "b"]);
    // 下一次没有 history 的帧不能再写账本。
    provider.history = None;
    let output = writer
        .render(&mut provider, size_80x24(), &mut sink)
        .unwrap();
    assert_eq!(output.history_rows_written, 0);
}

#[test]
fn overlay_composites_into_viewport_not_history() {
    let mut provider = StaticProvider {
        history: Some(HistoryBatch::append(1, vec!["hist".to_string()])),
        viewport: vec!["base".to_string()],
    };
    let mut writer = FrameWriter::new(FrameWriterOptions::default());
    writer.set_overlay(Some(OverlayFrame {
        row: 0,
        column: 0,
        lines: vec!["OVERLAY".to_string()],
        full_screen: false,
    }));
    let mut sink = StringSink::default();
    writer
        .render(&mut provider, size_80x24(), &mut sink)
        .unwrap();
    assert_eq!(writer.committed_rows(), ["hist"]);
    assert_eq!(writer.viewport_snapshot(), ["OVERLAY"]);
    assert!(
        !writer
            .committed_rows()
            .iter()
            .any(|row| row.contains("OVERLAY"))
    );
}

#[test]
fn alt_screen_golden_and_history_is_never_acked() {
    let mut provider = PendingProvider {
        batch: HistoryBatch::append(9, vec!["history".to_string()]),
        viewport: vec!["row1".to_string(), "row2".to_string()],
        ignore_ack: false,
        acks: Vec::new(),
    };
    let mut writer = FrameWriter::new(FrameWriterOptions {
        mode: TerminalMode::AltScreen,
        multiplexer: Multiplexer::Direct,
        synchronized_output: None,
    });
    let mut sink = StringSink::default();
    let output = writer
        .render(
            &mut provider,
            ViewportSize {
                columns: 10,
                rows: 5,
            },
            &mut sink,
        )
        .unwrap();
    assert!(output.bytes.starts_with(ENTER_ALT_SCREEN));
    assert!(
        output
            .bytes
            .starts_with(&format!("{ENTER_ALT_SCREEN}{BEGIN_SYNCHRONIZED_OUTPUT}"))
    );
    assert!(
        output
            .bytes
            .ends_with(&format!("{END_SYNCHRONIZED_OUTPUT}"))
    );
    assert!(output.bytes.contains("row1"));
    assert_eq!(output.history_rows_written, 0);
    assert!(provider.acks.is_empty(), "备用屏不接受 history");
    assert_eq!(writer.accepted_history_id(), 0);
    assert!(writer.committed_rows().is_empty());

    let mut stop_sink = StringSink::default();
    writer.stop(&mut stop_sink).unwrap();
    assert_eq!(stop_sink.output, EXIT_ALT_SCREEN);
}

#[test]
fn screen_mode_has_fixed_golden_without_synchronized_output() {
    let mut provider = StaticProvider {
        history: None,
        viewport: vec!["x".to_string()],
    };
    let mut writer = FrameWriter::new(FrameWriterOptions {
        mode: TerminalMode::Inline,
        multiplexer: Multiplexer::Screen,
        synchronized_output: None,
    });
    let mut sink = StringSink::default();
    writer
        .render(
            &mut provider,
            ViewportSize {
                columns: 10,
                rows: 24,
            },
            &mut sink,
        )
        .unwrap();
    let expected = "\x1b[1;1Hx\x1b[K\x1b[2;1H\x1b[J\x1b[1;1H\x1b[?25l";
    assert_eq!(sink.output, expected);
}

#[test]
fn tmux_mode_wraps_frame_in_synchronized_output() {
    let mut provider = StaticProvider {
        history: None,
        viewport: vec!["x".to_string()],
    };
    let mut writer = FrameWriter::new(FrameWriterOptions {
        mode: TerminalMode::Inline,
        multiplexer: Multiplexer::Tmux,
        synchronized_output: None,
    });
    let mut sink = StringSink::default();
    writer
        .render(
            &mut provider,
            ViewportSize {
                columns: 10,
                rows: 24,
            },
            &mut sink,
        )
        .unwrap();
    let inner = "\x1b[1;1Hx\x1b[K\x1b[2;1H\x1b[J\x1b[1;1H\x1b[?25l";
    assert_eq!(
        sink.output,
        format!("{BEGIN_SYNCHRONIZED_OUTPUT}{inner}{END_SYNCHRONIZED_OUTPUT}")
    );
}

#[test]
fn overwide_row_is_truncated_not_panicking() {
    let mut provider = StaticProvider {
        history: None,
        viewport: vec!["abcdefghijklmnop".to_string()],
    };
    let mut writer = FrameWriter::new(FrameWriterOptions {
        mode: TerminalMode::Inline,
        multiplexer: Multiplexer::Screen,
        synchronized_output: None,
    });
    let mut sink = StringSink::default();
    writer
        .render(
            &mut provider,
            ViewportSize {
                columns: 5,
                rows: 3,
            },
            &mut sink,
        )
        .unwrap();
    assert_eq!(
        kiana_tui::text::strip_terminal_sequences(&writer.viewport_snapshot()[0]),
        "abcde"
    );
    assert!(visible_width(&writer.viewport_snapshot()[0]) <= 5);
}

#[test]
fn history_appends_anchor_viewport_below_retained_rows() {
    let mut provider = StaticProvider {
        history: Some(HistoryBatch::append(
            1,
            vec!["h1".to_string(), "h2".to_string()],
        )),
        viewport: vec!["v1".to_string(), "v2".to_string()],
    };
    let mut writer = FrameWriter::new(FrameWriterOptions {
        mode: TerminalMode::Inline,
        multiplexer: Multiplexer::Screen,
        synchronized_output: None,
    });
    let mut sink = StringSink::default();
    let output = writer
        .render(
            &mut provider,
            ViewportSize {
                columns: 10,
                rows: 10,
            },
            &mut sink,
        )
        .unwrap();
    assert_eq!(output.viewport_top, 2, "viewport 锚定在 2 行 history 之后");
    assert_eq!(writer.viewport_snapshot(), ["v1", "v2"]);
}

#[test]
fn ten_thousand_line_transcript_survives_resize_cycles() {
    let mut ledger = TranscriptLedger::new();
    for index in 0..10_000 {
        ledger.add_block(vec![format!("line-{index}")]);
    }
    ledger.settle_all();
    let mut provider = TranscriptFrameProvider::new(ledger);
    let mut writer = FrameWriter::new(FrameWriterOptions {
        mode: TerminalMode::Inline,
        multiplexer: Multiplexer::Screen,
        synchronized_output: None,
    });
    let mut sink = StringSink::default();

    let mut frames = 0usize;
    loop {
        let output = writer
            .render(&mut provider, size_80x24(), &mut sink)
            .unwrap();
        frames += 1;
        assert!(frames < 1_000, "退休必须有进展");
        if output.history_rows_written == 0 {
            break;
        }
        if frames % 7 == 0 {
            writer
                .render(
                    &mut provider,
                    ViewportSize {
                        columns: 120,
                        rows: 40,
                    },
                    &mut sink,
                )
                .unwrap();
            writer
                .render(&mut provider, size_80x24(), &mut sink)
                .unwrap();
        }
    }

    // 模拟 resize：宽度变化后 provider 重排 live 尾部，已提交行保持不变。
    writer
        .render(
            &mut provider,
            ViewportSize {
                columns: 120,
                rows: 40,
            },
            &mut sink,
        )
        .unwrap();
    writer
        .render(&mut provider, size_80x24(), &mut sink)
        .unwrap();

    let mut logical: Vec<String> = writer
        .committed_rows()
        .iter()
        .chain(writer.viewport_snapshot().iter())
        .filter(|row| !row.is_empty())
        .cloned()
        .collect();
    logical.dedup();
    let expected: Vec<String> = (0..10_000).map(|index| format!("line-{index}")).collect();
    assert_eq!(logical, expected, "resize 后内容必须与逻辑 transcript 一致");
}
