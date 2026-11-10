//! `program-status-reporter.test.ts` 的移植。
//!
//! 覆盖 spec/ca-interactive-mode.md §2.11：7 个用例 + OSC 7501 编码断言。

use std::cell::RefCell;
use std::rc::Rc;

use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::Message;
use kiana_ai::types::{AssistantMessage, AssistantRole, StopReason};
use kiana_ext_api::events::CompactionReason;
use kiana_interactive::program_status_reporter::{BlockedStatus, ProgramStatusReporter};
use kiana_session::agent_session::AgentSessionEvent;
use kiana_tui::program_status::format_program_status;
use kiana_tui::terminal_colors::{ProgramStatus, ProgramStatusKind, ProgramStatusState, Terminal};
use serde_json::{Map, Value, json};

use crate::common::TestTerminal;

mod common;

struct Setup {
    reporter: ProgramStatusReporter,
    reports: Rc<RefCell<Vec<ProgramStatus>>>,
    session_name: Rc<RefCell<Option<String>>>,
}

#[allow(clippy::type_complexity)]
fn setup(session_name: Option<&str>) -> Setup {
    let terminal = TestTerminal::new();
    let reports = terminal.program_status_handle();
    let terminal: Rc<RefCell<Box<dyn Terminal>>> = Rc::new(RefCell::new(Box::new(terminal)));
    let session_name = Rc::new(RefCell::new(session_name.map(str::to_string)));
    let get_terminal: Rc<dyn Fn() -> Rc<RefCell<Box<dyn Terminal>>>> = {
        let terminal = Rc::clone(&terminal);
        Rc::new(move || Rc::clone(&terminal))
    };
    let get_name = {
        let session_name = Rc::clone(&session_name);
        Rc::new(move || session_name.borrow().clone())
    };
    let reporter = ProgramStatusReporter::new(
        Box::new({
            let get_terminal = Rc::clone(&get_terminal);
            move || get_terminal()
        }),
        Box::new({
            let get_name = Rc::clone(&get_name);
            move || get_name()
        }),
    );
    Setup {
        reporter,
        reports,
        session_name,
    }
}

fn send(reporter: &mut ProgramStatusReporter, events: impl IntoIterator<Item = AgentSessionEvent>) {
    for event in events {
        reporter.handle_event(&event);
    }
}

/// TS 测试的 `last()`：去掉 `app` 后按 TS 的键序比较。
fn last(reports: &Rc<RefCell<Vec<ProgramStatus>>>) -> Value {
    let status = reports.borrow().last().expect("a report").clone();
    let mut map = Map::new();
    map.insert(
        "state".to_string(),
        Value::String(kiana_tui::program_status::state_name(status.state).to_string()),
    );
    if let Some(kind) = status.kind
        && status.state == ProgramStatusState::Blocked
    {
        map.insert(
            "kind".to_string(),
            Value::String(kiana_tui::program_status::kind_name(kind).to_string()),
        );
    }
    if let Some(message) = status.message {
        map.insert("message".to_string(), Value::String(message));
    }
    Value::Object(map)
}

fn assistant_end(stop_reason: StopReason, error_message: Option<&str>) -> AgentSessionEvent {
    let message = AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![kiana_ai::types::AssistantContentBlock::Text(
            kiana_ai::types::TextContent {
                kind_text: kiana_ai::types::TextKind::Text,
                text: "secret assistant output".to_string(),
                text_signature: None,
            },
        )],
        stop_reason,
        error_message: error_message.map(str::to_string),
        ..Default::default()
    };
    AgentSessionEvent::MessageEnd {
        message: AgentMessage::Standard(Message::Assistant(message)),
    }
}

fn settled(aborted: bool) -> AgentSessionEvent {
    AgentSessionEvent::AgentSettled { aborted }
}

fn compaction_end(
    reason: CompactionReason,
    aborted: bool,
    error_message: Option<&str>,
) -> AgentSessionEvent {
    AgentSessionEvent::CompactionEnd {
        reason,
        result: None,
        aborted,
        will_retry: false,
        error_message: error_message.map(str::to_string),
    }
}

#[test]
fn reports_idle_working_during_a_run_and_done_once_it_settles() {
    let mut setup = setup(Some("Fix login"));
    setup.reporter.report();
    assert_eq!(last(&setup.reports), json!({"state": "idle"}));

    send(&mut setup.reporter, [AgentSessionEvent::AgentStart]);
    assert_eq!(
        last(&setup.reports),
        json!({"state": "working", "message": "Fix login"})
    );

    send(
        &mut setup.reporter,
        [
            assistant_end(StopReason::ToolUse, None),
            assistant_end(StopReason::Stop, None),
        ],
    );
    assert_eq!(
        last(&setup.reports),
        json!({"state": "working", "message": "Fix login"})
    );

    send(&mut setup.reporter, [settled(false)]);
    assert_eq!(
        last(&setup.reports),
        json!({"state": "done", "message": "Fix login"})
    );

    let serialized = serde_json::to_string(&*setup.reports.borrow()).unwrap();
    assert!(!serialized.contains("secret assistant output"));
}

#[test]
fn reports_only_the_outcome_of_the_run() {
    let mut setup = setup(None);
    send(
        &mut setup.reporter,
        [
            AgentSessionEvent::AgentStart,
            assistant_end(StopReason::Error, Some("overloaded")),
            assistant_end(StopReason::Stop, None),
            settled(false),
        ],
    );
    assert_eq!(last(&setup.reports), json!({"state": "done"}));

    send(
        &mut setup.reporter,
        [
            AgentSessionEvent::AgentStart,
            assistant_end(StopReason::Error, Some("Invalid API key\n{details}")),
            settled(false),
        ],
    );
    assert_eq!(
        last(&setup.reports),
        json!({"state": "error", "message": "Invalid API key"})
    );

    send(
        &mut setup.reporter,
        [
            AgentSessionEvent::AgentStart,
            assistant_end(StopReason::Aborted, None),
            settled(true),
        ],
    );
    assert_eq!(last(&setup.reports), json!({"state": "idle"}));

    // 成功响应之后被中止（例如 agent_before_settle 钩子或重试延迟）。
    send(
        &mut setup.reporter,
        [
            AgentSessionEvent::AgentStart,
            assistant_end(StopReason::Stop, None),
            settled(true),
        ],
    );
    assert_eq!(last(&setup.reports), json!({"state": "idle"}));
}

#[test]
fn reports_a_failed_recovery_compaction_as_the_run_error() {
    let mut setup = setup(None);
    send(
        &mut setup.reporter,
        [
            AgentSessionEvent::AgentStart,
            assistant_end(StopReason::Length, None),
            AgentSessionEvent::CompactionStart {
                reason: CompactionReason::Overflow,
            },
            compaction_end(
                CompactionReason::Overflow,
                false,
                Some("Compaction failed\nstack"),
            ),
            settled(false),
        ],
    );
    assert_eq!(
        last(&setup.reports),
        json!({"state": "error", "message": "Compaction failed"})
    );

    send(
        &mut setup.reporter,
        [
            AgentSessionEvent::AgentStart,
            AgentSessionEvent::CompactionStart {
                reason: CompactionReason::Threshold,
            },
            compaction_end(
                CompactionReason::Threshold,
                false,
                Some("Compaction failed"),
            ),
            assistant_end(StopReason::Stop, None),
            settled(false),
        ],
    );
    assert_eq!(last(&setup.reports), json!({"state": "done"}));
}

#[test]
fn reports_compaction_inside_a_run_and_manual_compaction_result() {
    let mut setup = setup(Some("Session"));
    send(
        &mut setup.reporter,
        [
            AgentSessionEvent::AgentStart,
            AgentSessionEvent::CompactionStart {
                reason: CompactionReason::Threshold,
            },
        ],
    );
    assert_eq!(
        last(&setup.reports),
        json!({"state": "working", "message": "Compacting context"})
    );
    send(
        &mut setup.reporter,
        [compaction_end(CompactionReason::Threshold, false, None)],
    );
    assert_eq!(
        last(&setup.reports),
        json!({"state": "working", "message": "Session"})
    );
    send(
        &mut setup.reporter,
        [assistant_end(StopReason::Stop, None), settled(false)],
    );

    send(
        &mut setup.reporter,
        [
            AgentSessionEvent::CompactionStart {
                reason: CompactionReason::Manual,
            },
            compaction_end(CompactionReason::Manual, false, None),
        ],
    );
    assert_eq!(
        last(&setup.reports),
        json!({"state": "done", "message": "Session"})
    );
    send(
        &mut setup.reporter,
        [
            AgentSessionEvent::CompactionStart {
                reason: CompactionReason::Manual,
            },
            compaction_end(CompactionReason::Manual, false, Some("No model")),
        ],
    );
    assert_eq!(
        last(&setup.reports),
        json!({"state": "error", "message": "No model"})
    );
    send(
        &mut setup.reporter,
        [
            AgentSessionEvent::CompactionStart {
                reason: CompactionReason::Manual,
            },
            compaction_end(CompactionReason::Manual, true, None),
        ],
    );
    assert_eq!(last(&setup.reports), json!({"state": "idle"}));

    // `errorMessage` 是空串时是 JS 假值：手动压缩按成功结束。
    send(
        &mut setup.reporter,
        [
            AgentSessionEvent::CompactionStart {
                reason: CompactionReason::Manual,
            },
            compaction_end(CompactionReason::Manual, false, Some("")),
        ],
    );
    assert_eq!(
        last(&setup.reports),
        json!({"state": "done", "message": "Session"})
    );
}

#[test]
fn reports_the_most_recent_open_dialog_and_the_underlying_state() {
    let mut setup = setup(None);
    send(&mut setup.reporter, [AgentSessionEvent::AgentStart]);
    setup.reporter.set_blocked(
        "extension-selector",
        Some(BlockedStatus {
            kind: ProgramStatusKind::Permission,
            message: "Allow bash?".to_string(),
        }),
    );
    setup.reporter.set_blocked(
        "login",
        Some(BlockedStatus {
            kind: ProgramStatusKind::Auth,
            message: "Log in to Anthropic".to_string(),
        }),
    );
    assert_eq!(
        last(&setup.reports),
        json!({"state": "blocked", "kind": "auth", "message": "Log in to Anthropic"})
    );

    // run 在 selector 仍打开时结束。
    setup.reporter.set_blocked("login", None);
    send(
        &mut setup.reporter,
        [assistant_end(StopReason::Stop, None), settled(false)],
    );
    assert_eq!(
        last(&setup.reports),
        json!({"state": "blocked", "kind": "permission", "message": "Allow bash?"})
    );

    // 重新打开同一个 source 会替换它，而不是再叠一个。
    setup.reporter.set_blocked(
        "extension-selector",
        Some(BlockedStatus {
            kind: ProgramStatusKind::Question,
            message: "Pick one".to_string(),
        }),
    );
    assert_eq!(
        last(&setup.reports),
        json!({"state": "blocked", "kind": "question", "message": "Pick one"})
    );
    setup.reporter.set_blocked("extension-selector", None);
    assert_eq!(last(&setup.reports), json!({"state": "done"}));
}

#[test]
fn sends_each_status_once_and_follows_session_name_changes() {
    let mut setup = setup(Some("Old"));
    send(
        &mut setup.reporter,
        [
            AgentSessionEvent::AgentStart,
            AgentSessionEvent::TurnStart,
            assistant_end(StopReason::ToolUse, None),
        ],
    );
    setup.reporter.report();
    assert_eq!(setup.reports.borrow().len(), 1);

    *setup.session_name.borrow_mut() = Some("New".to_string());
    send(
        &mut setup.reporter,
        [AgentSessionEvent::SessionInfoChanged {
            name: Some("New".to_string()),
        }],
    );
    assert_eq!(
        last(&setup.reports),
        json!({"state": "working", "message": "New"})
    );
}

#[test]
fn returns_to_idle_when_the_session_is_replaced() {
    let mut setup = setup(None);
    send(
        &mut setup.reporter,
        [
            AgentSessionEvent::AgentStart,
            assistant_end(StopReason::Stop, None),
            settled(false),
        ],
    );
    setup.reporter.reset();
    assert_eq!(last(&setup.reports), json!({"state": "idle"}));
}

#[test]
fn reports_osc7501_in_protocol_key_order() {
    let mut setup = setup(Some("Fix login"));
    send(&mut setup.reporter, [AgentSessionEvent::AgentStart]);
    setup.reporter.set_blocked(
        "login",
        Some(BlockedStatus {
            kind: ProgramStatusKind::Auth,
            message: "Log in to Anthropic".to_string(),
        }),
    );

    let reports = setup.reports.borrow();
    let encoded: Vec<String> = reports.iter().map(format_program_status).collect();
    assert_eq!(
        encoded,
        vec![
            // idle（report() 之前的显式 report 不在 setup 里，这里只有 agent_start + blocked）
            "\x1b]7501;state=working:app=kiana:msg=Rml4IGxvZ2lu\x1b\\".to_string(),
            "\x1b]7501;state=blocked:app=kiana:kind=auth:msg=TG9nIGluIHRvIEFudGhyb3BpYw==\x1b\\"
                .to_string(),
        ]
    );

    // 去重：同样的状态再次上报不会新增记录。
    drop(reports);
    setup.reporter.report();
    assert_eq!(setup.reports.borrow().len(), 2);
}
