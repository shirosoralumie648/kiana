//! modal 仲裁测试：单 modal 不变量、审批 1/N、settings 抢占不丢队列、
//! ask-user 单选/多选/All/Other/文本/yes-no、键盘与鼠标/滚动隔离。

use kiana_interactive::render_core::approval::{ApprovalCard, ApprovalDecision, ApprovalRequest};
use kiana_interactive::render_core::frame::{
    FrameProvider, FrameWriter, FrameWriterOptions, HistoryBatch, Multiplexer, StringSink,
    TerminalFramePlan, TerminalMode, ViewportSize,
};
use kiana_interactive::render_core::i18n::{Translator, builtin_pack};
use kiana_interactive::render_core::modal::{
    AskUserAnswer, AskUserDialog, AskUserOption, AskUserQuestion, InputEvent, ModalArbiter,
    ModalKey, ModalKind, ModalResolution, Routed, SettingsModal,
};
use serde_json::json;

fn card(id: &str) -> ApprovalCard {
    ApprovalCard::build(&ApprovalRequest::new("bash", json!({"command": "ls"})).with_id(id))
}

fn chinese_translator() -> Translator {
    let zh = builtin_pack("zh").expect("builtin zh");
    let en = builtin_pack("en").expect("builtin en");
    Translator::new(zh, en)
}

#[test]
fn localized_modal_headers_and_hints_follow_the_translator() {
    // 审批：审批 1/2。
    let mut arbiter = ModalArbiter::new();
    arbiter.set_translator(chinese_translator());
    arbiter.enqueue_approval(card("a"));
    arbiter.enqueue_approval(card("b"));
    let lines = arbiter.render(80);
    assert!(lines[0].contains("审批 1/2"), "{lines:?}");

    // ask-user：问题 1/2，多选提示走 modal.multi_select_hint。
    let mut arbiter = ModalArbiter::new();
    arbiter.set_translator(chinese_translator());
    arbiter.enqueue_ask_user(AskUserDialog::new(vec![
        AskUserQuestion::Single {
            prompt: "One".to_string(),
            options: vec![AskUserOption::new("A")],
        },
        AskUserQuestion::YesNo {
            prompt: "Two".to_string(),
            default_yes: false,
        },
    ]));
    let lines = arbiter.render(80);
    assert!(lines[0].contains("问题 1/2"), "{lines:?}");

    let mut arbiter = ModalArbiter::new();
    arbiter.set_translator(chinese_translator());
    arbiter.enqueue_ask_user(AskUserDialog::new(vec![AskUserQuestion::Multi {
        prompt: "Pick many".to_string(),
        options: vec![AskUserOption::new("A")],
        allow_all: true,
        allow_other: false,
    }]));
    let rendered = arbiter.render(80).join("\n");
    assert!(rendered.contains("空格切换，a 全选"), "{rendered}");
    assert!(rendered.contains("问题 1/1"), "{rendered}");

    // 设置：设置。
    let mut arbiter = ModalArbiter::new();
    arbiter.set_translator(chinese_translator());
    arbiter.open_settings(SettingsModal {
        lines: vec!["theme = dark".to_string()],
    });
    let lines = arbiter.render(80);
    assert!(lines[0].contains("设置"), "{lines:?}");
}

/// 未设置翻译器时，英文输出保持逐字一致。
#[test]
fn modal_without_translator_keeps_english_headers() {
    let mut arbiter = ModalArbiter::new();
    arbiter.enqueue_approval(card("a"));
    assert!(arbiter.render(80)[0].contains("Approval 1/1"));

    let mut arbiter = ModalArbiter::new();
    arbiter.open_settings(SettingsModal::default());
    assert!(arbiter.render(80)[0].contains("Settings"));
}

#[test]
fn three_queued_approvals_render_one_modal_with_1_of_3() {
    let mut arbiter = ModalArbiter::new();
    arbiter.enqueue_approval(card("a"));
    arbiter.enqueue_approval(card("b"));
    arbiter.enqueue_approval(card("c"));
    assert_eq!(arbiter.visible_modal_count(), 1);
    assert_eq!(arbiter.active_kind(), Some(ModalKind::Approval));
    assert_eq!(arbiter.approval_position(), Some((1, 3)));
    let lines = arbiter.render(80);
    assert!(lines[0].contains("Approval 1/3"), "{lines:?}");

    let resolution = arbiter
        .resolve_active(ApprovalDecision::Reject { reason: None })
        .unwrap();
    assert!(matches!(resolution, ModalResolution::Approval(_)));
    assert_eq!(arbiter.approval_position(), Some((1, 2)));
    assert_eq!(arbiter.visible_modal_count(), 1);
    assert_eq!(arbiter.active_approval_mut().unwrap().request_id, "b");
}

#[test]
fn settings_preempts_without_losing_approvals() {
    let mut arbiter = ModalArbiter::new();
    arbiter.enqueue_approval(card("first"));
    arbiter.enqueue_approval(card("second"));
    arbiter.open_settings(SettingsModal {
        lines: vec!["theme = dark".to_string()],
    });
    assert_eq!(arbiter.active_kind(), Some(ModalKind::Settings));
    assert!(arbiter.settings_open());
    assert_eq!(arbiter.visible_modal_count(), 1);
    assert_eq!(arbiter.approval_count(), 2, "权限项不能丢");

    let resolution = arbiter.close_settings();
    assert_eq!(resolution, Some(ModalResolution::SettingsClosed));
    assert_eq!(arbiter.active_kind(), Some(ModalKind::Approval));
    assert_eq!(arbiter.approval_position(), Some((1, 2)));
    assert_eq!(arbiter.active_approval_mut().unwrap().request_id, "first");
}

#[test]
fn only_one_modal_visible_at_every_step() {
    let mut arbiter = ModalArbiter::new();
    for index in 0..20 {
        arbiter.enqueue_approval(card(&format!("a{index}")));
        assert!(arbiter.visible_modal_count() <= 1);
    }
    arbiter.open_settings(SettingsModal::default());
    assert!(arbiter.visible_modal_count() <= 1);
    arbiter.close_settings();
    assert!(arbiter.visible_modal_count() <= 1);
    while arbiter.approval_count() > 0 {
        arbiter
            .resolve_active(ApprovalDecision::Reject { reason: None })
            .unwrap();
        assert!(arbiter.visible_modal_count() <= 1);
    }
}

#[test]
fn modal_isolates_mouse_scroll_and_resize_from_underlying_ui() {
    let mut arbiter = ModalArbiter::new();
    let mut underlying_events = 0usize;
    let route =
        |arbiter: &mut ModalArbiter, event: InputEvent, underlying: &mut usize| match arbiter
            .route_input(event)
        {
            Routed::Underlying => {
                *underlying += 1;
                "underlying"
            }
            Routed::Modal(_) => "modal",
            Routed::Ignored => "ignored",
        };

    assert_eq!(
        route(
            &mut arbiter,
            InputEvent::Key(ModalKey::Enter),
            &mut underlying_events
        ),
        "underlying"
    );
    assert_eq!(underlying_events, 1);

    arbiter.enqueue_approval(card("a"));
    assert_eq!(
        route(&mut arbiter, InputEvent::Mouse, &mut underlying_events),
        "ignored"
    );
    assert_eq!(
        route(&mut arbiter, InputEvent::Scroll(3), &mut underlying_events),
        "ignored"
    );
    assert_eq!(
        route(&mut arbiter, InputEvent::Resize, &mut underlying_events),
        "ignored"
    );
    assert_eq!(underlying_events, 1, "底层不能收到模态期间的事件");
    assert_eq!(
        route(
            &mut arbiter,
            InputEvent::Key(ModalKey::Escape),
            &mut underlying_events
        ),
        "modal"
    );
    assert_eq!(
        route(
            &mut arbiter,
            InputEvent::Key(ModalKey::Enter),
            &mut underlying_events
        ),
        "underlying"
    );
}

#[test]
fn ask_user_single_select() {
    let mut arbiter = ModalArbiter::new();
    arbiter.enqueue_ask_user(AskUserDialog::new(vec![AskUserQuestion::Single {
        prompt: "Pick one".to_string(),
        options: vec![AskUserOption::new("A"), AskUserOption::new("B")],
    }]));
    assert_eq!(arbiter.active_kind(), Some(ModalKind::AskUser));
    let _ = arbiter.route_input(InputEvent::Key(ModalKey::Down));
    let routed = arbiter.route_input(InputEvent::Key(ModalKey::Enter));
    match routed {
        Routed::Modal(Some(ModalResolution::AskUser(answers))) => {
            assert_eq!(
                answers,
                vec![AskUserAnswer::Single {
                    index: Some(1),
                    label: "B".to_string(),
                    other: None,
                }]
            );
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn cancelling_ask_user_does_not_swallow_later_input() {
    let mut arbiter = ModalArbiter::new();
    arbiter.enqueue_ask_user(AskUserDialog::new(vec![AskUserQuestion::YesNo {
        prompt: "Proceed?".to_string(),
        default_yes: true,
    }]));
    arbiter.enqueue_approval(card("queued"));
    assert_eq!(arbiter.active_kind(), Some(ModalKind::AskUser));

    let routed = arbiter.route_input(InputEvent::Key(ModalKey::Escape));
    assert!(matches!(routed, Routed::Modal(None)), "{routed:?}");
    assert_eq!(
        arbiter.active_kind(),
        Some(ModalKind::Approval),
        "queued approval must surface after the ask-user dialog is cancelled"
    );
    let routed = arbiter.route_input(InputEvent::Key(ModalKey::Enter));
    assert!(
        matches!(routed, Routed::Modal(Some(ModalResolution::Approval(_)))),
        "{routed:?}"
    );
}

#[test]
fn ask_user_multi_supports_all_and_other() {
    let mut arbiter = ModalArbiter::new();
    arbiter.enqueue_ask_user(AskUserDialog::new(vec![AskUserQuestion::Multi {
        prompt: "Pick many".to_string(),
        options: vec![AskUserOption::new("A"), AskUserOption::new("B")],
        allow_all: true,
        allow_other: true,
    }]));
    let _ = arbiter.route_input(InputEvent::Key(ModalKey::Char('a')));
    match arbiter.route_input(InputEvent::Key(ModalKey::Enter)) {
        Routed::Modal(Some(ModalResolution::AskUser(answers))) => {
            assert_eq!(
                answers,
                vec![AskUserAnswer::Multi {
                    indices: vec![0, 1],
                    labels: vec!["A".to_string(), "B".to_string()],
                    other: None,
                }],
                "got {answers:?}"
            );
        }
        other => panic!("unexpected {other:?}"),
    }

    // 先按 'o' 进入 Other，再输入文本。
    let mut arbiter = ModalArbiter::new();
    arbiter.enqueue_ask_user(AskUserDialog::new(vec![AskUserQuestion::Multi {
        prompt: "Pick many".to_string(),
        options: vec![AskUserOption::new("A"), AskUserOption::new("B")],
        allow_all: false,
        allow_other: true,
    }]));
    let _ = arbiter.route_input(InputEvent::Key(ModalKey::Char('o')));
    for character in "custom".chars() {
        let _ = arbiter.route_input(InputEvent::Key(ModalKey::Char(character)));
    }
    match arbiter.route_input(InputEvent::Key(ModalKey::Enter)) {
        Routed::Modal(Some(ModalResolution::AskUser(answers))) => {
            assert_eq!(
                answers,
                vec![AskUserAnswer::Multi {
                    indices: Vec::new(),
                    labels: Vec::new(),
                    other: Some("custom".to_string()),
                }]
            );
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn ask_user_text_and_yes_no() {
    let mut arbiter = ModalArbiter::new();
    arbiter.enqueue_ask_user(AskUserDialog::new(vec![AskUserQuestion::Text {
        prompt: "Name?".to_string(),
        placeholder: Some("type here".to_string()),
    }]));
    for character in "kiana".chars() {
        let _ = arbiter.route_input(InputEvent::Key(ModalKey::Char(character)));
    }
    match arbiter.route_input(InputEvent::Key(ModalKey::Enter)) {
        Routed::Modal(Some(ModalResolution::AskUser(answers))) => {
            assert_eq!(answers, vec![AskUserAnswer::Text("kiana".to_string())]);
        }
        other => panic!("unexpected {other:?}"),
    }

    let mut arbiter = ModalArbiter::new();
    arbiter.enqueue_ask_user(AskUserDialog::new(vec![AskUserQuestion::YesNo {
        prompt: "Continue?".to_string(),
        default_yes: false,
    }]));
    match arbiter.route_input(InputEvent::Key(ModalKey::Enter)) {
        Routed::Modal(Some(ModalResolution::AskUser(answers))) => {
            assert_eq!(answers, vec![AskUserAnswer::YesNo(false)]);
        }
        other => panic!("unexpected {other:?}"),
    }

    let mut arbiter = ModalArbiter::new();
    arbiter.enqueue_ask_user(AskUserDialog::new(vec![AskUserQuestion::YesNo {
        prompt: "Continue?".to_string(),
        default_yes: true,
    }]));
    match arbiter.route_input(InputEvent::Key(ModalKey::Enter)) {
        Routed::Modal(Some(ModalResolution::AskUser(answers))) => {
            assert_eq!(answers, vec![AskUserAnswer::YesNo(true)]);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn ask_user_multiple_questions_advance() {
    let mut arbiter = ModalArbiter::new();
    arbiter.enqueue_ask_user(AskUserDialog::new(vec![
        AskUserQuestion::Single {
            prompt: "One".to_string(),
            options: vec![AskUserOption::new("A")],
        },
        AskUserQuestion::YesNo {
            prompt: "Two".to_string(),
            default_yes: false,
        },
    ]));
    let first = arbiter.route_input(InputEvent::Key(ModalKey::Enter));
    assert_eq!(first, Routed::Modal(None));
    assert_eq!(arbiter.active_kind(), Some(ModalKind::AskUser));
    match arbiter.route_input(InputEvent::Key(ModalKey::Enter)) {
        Routed::Modal(Some(ModalResolution::AskUser(answers))) => {
            assert_eq!(answers.len(), 2);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn modal_overlay_never_enters_history() {
    let mut arbiter = ModalArbiter::new();
    arbiter.enqueue_approval(card("a"));
    let overlay = arbiter.overlay_frame(60).expect("overlay");
    assert!(
        overlay
            .lines
            .iter()
            .any(|line| line.contains("Approval 1/1"))
    );

    struct Provider {
        history_written: bool,
    }
    impl FrameProvider for Provider {
        fn render_frame(&mut self, _viewport: ViewportSize) -> TerminalFramePlan {
            let history = (!self.history_written).then(|| {
                self.history_written = true;
                HistoryBatch::append(1, vec!["committed".to_string()])
            });
            TerminalFramePlan {
                history,
                viewport: vec!["base".to_string()],
            }
        }
    }
    let mut writer = FrameWriter::new(FrameWriterOptions {
        mode: TerminalMode::Inline,
        multiplexer: Multiplexer::Screen,
        synchronized_output: None,
    });
    writer.set_overlay(Some(overlay));
    let mut sink = StringSink::default();
    writer
        .render(
            &mut Provider {
                history_written: false,
            },
            ViewportSize {
                columns: 60,
                rows: 10,
            },
            &mut sink,
        )
        .unwrap();
    assert_eq!(writer.committed_rows(), ["committed"]);
    assert!(writer.viewport_snapshot()[0].contains("Approval"));
}

#[test]
fn settings_and_ask_user_are_mutually_exclusive() {
    let mut arbiter = ModalArbiter::new();
    arbiter.enqueue_ask_user(AskUserDialog::new(vec![AskUserQuestion::Text {
        prompt: "q".to_string(),
        placeholder: None,
    }]));
    arbiter.open_settings(SettingsModal::default());
    assert_eq!(arbiter.visible_modal_count(), 1);
    assert_eq!(arbiter.active_kind(), Some(ModalKind::Settings));
    arbiter.close_settings();
    assert_eq!(arbiter.active_kind(), Some(ModalKind::AskUser));
}

#[test]
fn resolving_ask_user_with_approval_decision_is_rejected_without_losing_it() {
    let mut arbiter = ModalArbiter::new();
    arbiter.enqueue_ask_user(AskUserDialog::new(vec![AskUserQuestion::Text {
        prompt: "name".to_string(),
        placeholder: None,
    }]));
    assert!(
        arbiter
            .resolve_active(ApprovalDecision::Reject { reason: None })
            .is_none()
    );
    assert_eq!(arbiter.active_kind(), Some(ModalKind::AskUser));
    assert_eq!(arbiter.visible_modal_count(), 1);
}
