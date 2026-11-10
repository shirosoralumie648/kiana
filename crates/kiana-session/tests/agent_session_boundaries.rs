//! `test/suite/agent-session-boundaries.test.ts` 的 Rust 移植（精确仲裁/排队/结算用例）。
//!
//! 说明：TS 文件共有 29 个执行（26 个 it + 3 个参数化变体）。本文件覆盖仲裁/排队/结算
//! 用例；边界用量、压缩链与持久化长度恢复用例在 `agent_session_boundaries_extra.rs`。

mod common;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use common::*;
use kiana_ai::providers::faux::{FauxResponseStep, faux_assistant_message};
use kiana_ai::types::{StopReason, UserContent};
use kiana_ext_api::events::{
    AgentActivityOutcome, BoundaryResult, CompactionEntryDraft, CompactionEntryDraftType,
    ContextEditEntryDraft, ContextEditEntryDraftType, CustomEntryDraft, CustomEntryDraftType,
    CustomMessageEntryDraft, CustomMessageEntryDraftType, ExtensionEvent, ExtensionEventKind,
    ExtensionEventResult, SessionBoundaryDraft, SessionEvent,
};
use kiana_session_core::session_manager::{ContextEditReplacement, SessionEntry};
use serde_json::json;

fn draft_compaction(summary: &str, first_kept_entry_id: Option<&str>) -> SessionBoundaryDraft {
    SessionBoundaryDraft::Compaction(CompactionEntryDraft {
        event_type: CompactionEntryDraftType::Compaction,
        summary: summary.to_string(),
        first_kept_entry_id: first_kept_entry_id.map(str::to_string),
        details: Some(json!({ "source": "test" })),
        usage: None,
    })
}

fn draft_custom(custom_type: &str, data: serde_json::Value) -> SessionBoundaryDraft {
    SessionBoundaryDraft::Custom(CustomEntryDraft {
        event_type: CustomEntryDraftType::Custom,
        custom_type: custom_type.to_string(),
        data: Some(data),
    })
}

fn draft_custom_message(custom_type: &str, content: &str, display: bool) -> SessionBoundaryDraft {
    SessionBoundaryDraft::CustomMessage(CustomMessageEntryDraft {
        event_type: CustomMessageEntryDraftType::CustomMessage,
        custom_type: custom_type.to_string(),
        content: UserContent::Text(content.to_string()),
        display,
        details: None,
    })
}

fn draft_context_edit(target_id: &str, replacement: Option<&str>) -> SessionBoundaryDraft {
    SessionBoundaryDraft::ContextEdit(ContextEditEntryDraft {
        event_type: ContextEditEntryDraftType::ContextEdit,
        target_id: target_id.to_string(),
        replacement: replacement.map(|content| ContextEditReplacement {
            content: json!(content),
        }),
    })
}

fn turn_end_result(entries: Vec<SessionBoundaryDraft>, r#continue: bool) -> HandlerResult {
    Ok(Some(ExtensionEventResult::TurnEnd(BoundaryResult {
        entries: Some(entries),
        r#continue: Some(r#continue),
    })))
}

fn request_json(input: &kiana_ai::providers::faux::FauxResponseInput) -> String {
    serde_json::to_string(&input.context.as_context().messages).unwrap_or_default()
}

/// TS: "commits a retain-none turn_end compaction and explicitly continues once"
#[tokio::test]
async fn commits_a_retain_none_turn_end_compaction_and_explicitly_continues_once() {
    run(async {
        let handled = Arc::new(AtomicBool::new(false));
        let observed = Arc::new(Mutex::new(Vec::<String>::new()));
        let requests = Arc::new(Mutex::new(Vec::<String>::new()));
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory({
                let handled = Arc::clone(&handled);
                let observed = Arc::clone(&observed);
                move |kiana| {
                    let handled = Arc::clone(&handled);
                    let observed = Arc::clone(&observed);
                    let _ = kiana.on(
                        ExtensionEventKind::TurnEnd,
                        standard(move |event, _ctx| {
                            let handled = Arc::clone(&handled);
                            let observed = Arc::clone(&observed);
                            Box::pin(async move {
                                let ExtensionEvent::TurnEnd(turn) = event else {
                                    return Ok(None);
                                };
                                observed.lock().unwrap().push(turn.message_entry_id.clone());
                                if handled.swap(true, Ordering::SeqCst) {
                                    return Ok(None);
                                }
                                turn_end_result(vec![draft_compaction("exact handoff", None)], true)
                            })
                        }),
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        let requests_capture = Arc::clone(&requests);
        harness.set_responses(vec![
            faux_text_response("discarded response"),
            FauxResponseStep::message_factory(move |input| {
                let requests = Arc::clone(&requests_capture);
                async move {
                    requests.lock().unwrap().push(request_json(&input));
                    faux_assistant_message("continued from handoff", Default::default())
                }
            }),
        ]);
        harness
            .session
            .prompt("discarded prompt".to_string(), Default::default())
            .await
            .unwrap();
        let compaction = harness
            .session_entries()
            .into_iter()
            .find(|entry| entry.kind() == "compaction")
            .expect("compaction entry");
        if let SessionEntry::Compaction(entry) = &compaction {
            assert_eq!(entry.summary, "exact handoff");
            assert_eq!(entry.first_kept_entry_id, entry.id);
        }
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("exact handoff"));
        assert!(!requests[0].contains("discarded prompt"));
        assert!(!requests[0].contains("discarded response"));
        assert_eq!(observed.lock().unwrap().len(), 2);
        assert_eq!(harness.events_of_type("agent_settled").len(), 1);
        harness.cleanup();
    })
    .await;
}

async fn run_queue_scheduling(queue_kind: &'static str) {
    let handled = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let harness = create_harness(HarnessOptions {
        extension_factories: vec![factory({
            let handled = Arc::clone(&handled);
            move |kiana| {
                let handled = Arc::clone(&handled);
                let api = Arc::clone(&kiana);
                let _ = kiana.on(
                    ExtensionEventKind::TurnEnd,
                    standard(move |_event, _ctx| {
                        let handled = Arc::clone(&handled);
                        let api = Arc::clone(&api);
                        Box::pin(async move {
                            if handled.swap(true, Ordering::SeqCst) {
                                return Ok(None);
                            }
                            if queue_kind == "steering" || queue_kind == "both" {
                                api.send_user_message(
                                    UserContent::Text("queued steering".to_string()),
                                    Some(kiana_ext_api::types::SendUserMessageOptions {
                                        deliver_as: Some(
                                            kiana_ext_api::types::SendUserMessageDeliverAs::Steer,
                                        ),
                                        expand_prompt_templates: Some(false),
                                    }),
                                );
                            }
                            if queue_kind == "follow-up" || queue_kind == "both" {
                                api.send_user_message(
                                    UserContent::Text("queued follow-up".to_string()),
                                    Some(kiana_ext_api::types::SendUserMessageOptions {
                                        deliver_as: Some(
                                            kiana_ext_api::types::SendUserMessageDeliverAs::FollowUp,
                                        ),
                                        expand_prompt_templates: Some(false),
                                    }),
                                );
                            }
                            turn_end_result(vec![draft_compaction("exact handoff", None)], true)
                        })
                    }),
                );
            }
        })],
        ..Default::default()
    })
    .await;
    let first_capture = Arc::clone(&requests);
    harness.set_responses(vec![
        faux_text_response("first"),
        FauxResponseStep::message_factory(move |input| {
            let requests = Arc::clone(&first_capture);
            async move {
                requests.lock().unwrap().push(request_json(&input));
                faux_assistant_message("second", Default::default())
            }
        }),
        FauxResponseStep::message_factory({
            let requests = Arc::clone(&requests);
            move |input| {
                let requests = Arc::clone(&requests);
                async move {
                    requests.lock().unwrap().push(request_json(&input));
                    faux_assistant_message("third", Default::default())
                }
            }
        }),
    ]);
    harness
        .session
        .prompt("start".to_string(), Default::default())
        .await
        .unwrap();
    let requests = requests.lock().unwrap();
    assert!(requests[0].contains("exact handoff"));
    match queue_kind {
        "steering" => {
            assert_eq!(harness.faux.state().call_count(), 2);
            assert!(requests[0].contains("queued steering"));
            assert!(!requests[0].contains("queued follow-up"));
        }
        "follow-up" => {
            assert_eq!(harness.faux.state().call_count(), 2);
            assert!(requests[0].contains("queued follow-up"));
        }
        _ => {
            assert_eq!(harness.faux.state().call_count(), 3);
            assert!(requests[0].contains("queued steering"));
            assert!(!requests[0].contains("queued follow-up"));
            assert!(requests[1].contains("queued follow-up"));
        }
    }
    harness.cleanup();
}

/// TS: `it.each(["steering", "follow-up", "both"])("preserves %s queue scheduling around a turn_end handoff")`
#[tokio::test]
async fn preserves_steering_queue_scheduling_around_a_turn_end_handoff() {
    run(run_queue_scheduling("steering")).await;
}

/// TS: `preserves follow-up queue scheduling around a turn_end handoff`
#[tokio::test]
async fn preserves_follow_up_queue_scheduling_around_a_turn_end_handoff() {
    run(run_queue_scheduling("follow-up")).await;
}

/// TS: `preserves both queue scheduling around a turn_end handoff`
#[tokio::test]
async fn preserves_both_queue_scheduling_around_a_turn_end_handoff() {
    run(run_queue_scheduling("both")).await;
}

/// TS: "refreshes canonical context before publishing boundary entry notifications"
#[tokio::test]
async fn refreshes_canonical_context_before_publishing_boundary_entry_notifications() {
    run(async {
        let handled = Arc::new(AtomicBool::new(false));
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory({
                let handled = Arc::clone(&handled);
                move |kiana| {
                    let handled = Arc::clone(&handled);
                    let _ = kiana.on(
                        ExtensionEventKind::TurnEnd,
                        standard(move |_event, _ctx| {
                            let handled = Arc::clone(&handled);
                            Box::pin(async move {
                                if handled.swap(true, Ordering::SeqCst) {
                                    return Ok(None);
                                }
                                turn_end_result(
                                    vec![
                                        draft_custom("metadata", json!(true)),
                                        draft_custom_message(
                                            "visible-context",
                                            "committed context",
                                            true,
                                        ),
                                    ],
                                    false,
                                )
                            })
                        }),
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        let snapshots = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = Arc::clone(&snapshots);
        let session = harness.session.clone();
        let _unsubscribe = harness.session.subscribe(Arc::new(move |event| {
            if let kiana_session::AgentSessionEvent::EntryAppended { .. } = event {
                let messages = session.messages();
                sink.lock().unwrap().push(
                    serde_json::to_string(
                        &messages
                            .iter()
                            .map(|message| message.role().to_string())
                            .collect::<Vec<_>>(),
                    )
                    .unwrap_or_default()
                        + &format!("{messages:?}"),
                );
            }
        }));
        harness.set_responses(vec![faux_text_response("done")]);
        harness
            .session
            .prompt("start".to_string(), Default::default())
            .await
            .unwrap();
        let snapshots = snapshots.lock().unwrap();
        assert_eq!(snapshots.len(), 2);
        assert!(
            snapshots
                .iter()
                .all(|snapshot| snapshot.contains("committed context"))
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "continues from an agent_before_settle custom message before final settlement"
#[tokio::test]
async fn continues_from_agent_before_settle_custom_message_before_final_settlement() {
    run(async {
        let requested = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::<String>::new()));
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory({
                let requested = Arc::clone(&requested);
                move |kiana| {
                    let requested = Arc::clone(&requested);
                    let _ = kiana.on(
                        ExtensionEventKind::AgentBeforeSettle,
                        standard(move |_event, _ctx| {
                            let requested = Arc::clone(&requested);
                            Box::pin(async move {
                                if requested.swap(true, Ordering::SeqCst) {
                                    return Ok(None);
                                }
                                Ok(Some(ExtensionEventResult::AgentBeforeSettle(
                                    BoundaryResult {
                                        entries: Some(vec![draft_custom_message(
                                            "test-continuation",
                                            "continue now",
                                            false,
                                        )]),
                                        r#continue: Some(true),
                                    },
                                )))
                            })
                        }),
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        let capture = Arc::clone(&requests);
        harness.set_responses(vec![
            faux_text_response("first"),
            FauxResponseStep::message_factory(move |input| {
                let requests = Arc::clone(&capture);
                async move {
                    requests.lock().unwrap().push(request_json(&input));
                    faux_assistant_message("second", Default::default())
                }
            }),
        ]);
        harness
            .session
            .prompt("start".to_string(), Default::default())
            .await
            .unwrap();
        assert!(requests.lock().unwrap()[0].contains("continue now"));
        assert!(harness.session_entries().iter().any(|entry| {
            match entry {
                SessionEntry::CustomMessage(message) => {
                    message.custom_type == "test-continuation" && !message.display
                }
                _ => false,
            }
        }));
        assert_eq!(harness.events_of_type("agent_start").len(), 2);
        assert_eq!(harness.events_of_type("agent_settled").len(), 1);
        harness.cleanup();
    })
    .await;
}

/// TS: "persists custom context queued by agent_end before pre-settlement continuation"
#[tokio::test]
async fn persists_custom_context_queued_by_agent_end_before_pre_settlement_continuation() {
    run(async {
        let first_run = Arc::new(AtomicBool::new(true));
        let continued = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::<String>::new()));
        let saw_pending = Arc::new(AtomicBool::new(false));
        let saw_context = Arc::new(AtomicBool::new(false));
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory({
                let first_run = Arc::clone(&first_run);
                let continued = Arc::clone(&continued);
                let saw_pending = Arc::clone(&saw_pending);
                let saw_context = Arc::clone(&saw_context);
                move |kiana| {
                    let api = Arc::clone(&kiana);
                    let first_run = Arc::clone(&first_run);
                    let _ = kiana.on(
                        ExtensionEventKind::AgentEnd,
                        standard(move |_event, _ctx| {
                            let first_run = Arc::clone(&first_run);
                            let api = Arc::clone(&api);
                            Box::pin(async move {
                                if first_run.swap(false, Ordering::SeqCst) {
                                    api.send_message(kiana_ext_api::types::SendMessageOptions {
                                        message: kiana_ext_api::types::CustomMessagePayload {
                                            custom_type: "agent-end-context".to_string(),
                                            content: UserContent::Text(
                                                "queued after agent end".to_string(),
                                            ),
                                            display: false,
                                            details: None,
                                        },
                                        trigger_turn: Some(false),
                                        deliver_as: None,
                                    });
                                }
                                Ok(None)
                            })
                        }),
                    );
                    let continued = Arc::clone(&continued);
                    let saw_pending = Arc::clone(&saw_pending);
                    let saw_context = Arc::clone(&saw_context);
                    let _ = kiana.on(
                        ExtensionEventKind::AgentBeforeSettle,
                        standard(move |event, _ctx| {
                            let continued = Arc::clone(&continued);
                            let saw_pending = Arc::clone(&saw_pending);
                            let saw_context = Arc::clone(&saw_context);
                            Box::pin(async move {
                                if continued.swap(true, Ordering::SeqCst) {
                                    return Ok(None);
                                }
                                let ExtensionEvent::AgentBeforeSettle(settle) = event else {
                                    return Ok(None);
                                };
                                let pending =
                                    serde_json::to_string(&settle.context.pending_messages)
                                        .unwrap_or_default();
                                let context =
                                    serde_json::to_string(&settle.context.context_messages)
                                        .unwrap_or_default();
                                saw_pending.store(
                                    pending.contains("queued after agent end"),
                                    Ordering::SeqCst,
                                );
                                saw_context.store(
                                    !context.contains("queued after agent end"),
                                    Ordering::SeqCst,
                                );
                                Ok(Some(ExtensionEventResult::AgentBeforeSettle(
                                    BoundaryResult {
                                        entries: None,
                                        r#continue: Some(true),
                                    },
                                )))
                            })
                        }),
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        let capture = Arc::clone(&requests);
        harness.set_responses(vec![
            faux_text_response("first"),
            FauxResponseStep::message_factory(move |input| {
                let requests = Arc::clone(&capture);
                async move {
                    requests.lock().unwrap().push(request_json(&input));
                    faux_assistant_message("second", Default::default())
                }
            }),
        ]);
        harness
            .session
            .prompt("start".to_string(), Default::default())
            .await
            .unwrap();
        assert!(requests.lock().unwrap()[0].contains("queued after agent end"));
        assert!(harness.session_entries().iter().any(|entry| match entry {
            SessionEntry::CustomMessage(message) => message.custom_type == "agent-end-context",
            _ => false,
        }));
        assert!(saw_pending.load(Ordering::SeqCst));
        assert!(saw_context.load(Ordering::SeqCst));
        harness.cleanup();
    })
    .await;
}

/// TS: "defers runs started by agent_settled handlers until every settled handler completes"
#[tokio::test]
async fn defers_runs_started_by_agent_settled_handlers_until_every_settled_handler_completes() {
    run(async {
        let lifecycle = Arc::new(Mutex::new(Vec::<String>::new()));
        let triggered = Arc::new(AtomicBool::new(false));
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory({
                let lifecycle = Arc::clone(&lifecycle);
                let triggered = Arc::clone(&triggered);
                move |kiana| {
                    let start_lifecycle = Arc::clone(&lifecycle);
                    let _ = kiana.on(
                        ExtensionEventKind::AgentStart,
                        standard(move |_event, _ctx| {
                            let lifecycle = Arc::clone(&start_lifecycle);
                            Box::pin(async move {
                                lifecycle.lock().unwrap().push("start".to_string());
                                Ok(None)
                            })
                        }),
                    );
                    let api = Arc::clone(&kiana);
                    let first_lifecycle = Arc::clone(&lifecycle);
                    let triggered_flag = Arc::clone(&triggered);
                    let _ = kiana.on(
                        ExtensionEventKind::AgentSettled,
                        standard(move |_event, ctx| {
                            let lifecycle = Arc::clone(&first_lifecycle);
                            let api = Arc::clone(&api);
                            let triggered = Arc::clone(&triggered_flag);
                            Box::pin(async move {
                                lifecycle
                                    .lock()
                                    .unwrap()
                                    .push(format!("settled-first:{}", ctx.is_idle()));
                                if !triggered.swap(true, Ordering::SeqCst) {
                                    api.send_message(kiana_ext_api::types::SendMessageOptions {
                                        message: kiana_ext_api::types::CustomMessagePayload {
                                            custom_type: "settled-trigger".to_string(),
                                            content: UserContent::Text("start later".to_string()),
                                            display: false,
                                            details: None,
                                        },
                                        trigger_turn: Some(true),
                                        deliver_as: None,
                                    });
                                }
                                Ok(None)
                            })
                        }),
                    );
                    let second_lifecycle = Arc::clone(&lifecycle);
                    let _ = kiana.on(
                        ExtensionEventKind::AgentSettled,
                        standard(move |_event, ctx| {
                            let lifecycle = Arc::clone(&second_lifecycle);
                            Box::pin(async move {
                                lifecycle
                                    .lock()
                                    .unwrap()
                                    .push(format!("settled-second:{}", ctx.is_idle()));
                                Ok(None)
                            })
                        }),
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            faux_text_response("first"),
            faux_text_response("second"),
        ]);
        harness
            .session
            .prompt("start".to_string(), Default::default())
            .await
            .unwrap();
        assert_eq!(
            *lifecycle.lock().unwrap(),
            vec![
                "start".to_string(),
                "settled-first:true".to_string(),
                "settled-second:true".to_string(),
                "start".to_string(),
                "settled-first:true".to_string(),
                "settled-second:true".to_string(),
            ]
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "does not let an invalid explicit continuation suppress natural tool continuation"
#[tokio::test]
async fn does_not_let_invalid_explicit_continuation_suppress_natural_tool_continuation() {
    run(async {
        struct NoopTool;
        impl kiana_agent_core::types::AgentToolExecute for NoopTool {
            fn execute<'a>(
                &'a self,
                _tool_call_id: &'a str,
                _params: serde_json::Value,
                _signal: Option<&'a tokio_util::sync::CancellationToken>,
                _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
            ) -> kiana_agent_core::types::BoxFuture<
                'a,
                Result<
                    kiana_agent_core::types::AgentToolResult<serde_json::Value>,
                    kiana_agent_core::types::AgentToolError,
                >,
            > {
                Box::pin(async {
                    Ok(kiana_agent_core::types::AgentToolResult {
                        content: vec![kiana_ai::types::UserContentBlock::Text(
                            kiana_ai::types::TextContent {
                                kind_text: kiana_ai::types::TextKind::Text,
                                text: "done".to_string(),
                                text_signature: None,
                            },
                        )],
                        details: serde_json::json!({}),
                        structured_content: None,
                        usage: None,
                        is_error: None,
                        terminate: None,
                    })
                })
            }
        }
        let tool = kiana_agent_core::types::AgentTool {
            name: "noop".to_string(),
            label: "Noop".to_string(),
            description: "Noop".to_string(),
            parameters: json!({"type": "object", "properties": {}}),
            constrained_sampling: None,
            output_schema: None,
            prepare_arguments: None,
            execution_mode: None,
            replay: None,
            execute: Arc::new(NoopTool),
        };
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![tool]),
            extension_factories: vec![factory(|kiana| {
                let _ = kiana.on(
                    ExtensionEventKind::TurnEnd,
                    standard(move |event, ctx| {
                        Box::pin(async move {
                            let ExtensionEvent::TurnEnd(turn) = event else {
                                return Ok(None);
                            };
                            let branch = ctx.session_manager().get_branch(None);
                            let user = branch
                                .iter()
                                .rev()
                                .find(|entry| match entry {
                                    SessionEntry::Message(message) => {
                                        message.message.role() == "user"
                                    }
                                    _ => false,
                                })
                                .map(|entry| entry.id().to_string());
                            let mut targets = Vec::new();
                            if let Some(user) = user {
                                targets.push(user);
                            }
                            targets.push(turn.message_entry_id.clone());
                            targets.extend(turn.tool_result_entry_ids.clone());
                            turn_end_result(
                                targets
                                    .iter()
                                    .map(|target| draft_context_edit(target, None))
                                    .collect(),
                                true,
                            )
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            faux_tool_response(
                kiana_ai::types::ToolCall {
                    kind_tool_call: kiana_ai::types::ToolCallKind::ToolCall,
                    id: "call-1".to_string(),
                    name: "noop".to_string(),
                    arguments: Default::default(),
                    thought_signature: None,
                    namespace: None,
                    arguments_error: None,
                },
                StopReason::ToolUse,
            ),
            faux_text_response("must not run"),
        ]);
        harness
            .session
            .prompt("start".to_string(), Default::default())
            .await
            .unwrap();
        assert_eq!(harness.faux.state().call_count(), 2);
        harness.cleanup();
    })
    .await;
}

/// TS: "dispatches actionable turn_end for synthetic run failures"
#[tokio::test]
async fn dispatches_actionable_turn_end_for_synthetic_run_failures() {
    run(async {
        let turn_ends = Arc::new(AtomicU32::new(0));
        let outcomes = Arc::new(Mutex::new(Vec::<AgentActivityOutcome>::new()));
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory({
                let turn_ends = Arc::clone(&turn_ends);
                let outcomes = Arc::clone(&outcomes);
                move |kiana| {
                    let turn_ends = Arc::clone(&turn_ends);
                    let outcomes = Arc::clone(&outcomes);
                    let _ = kiana.on(
                        ExtensionEventKind::TurnEnd,
                        standard(move |event, _ctx| {
                            let turn_ends = Arc::clone(&turn_ends);
                            let outcomes = Arc::clone(&outcomes);
                            Box::pin(async move {
                                turn_ends.fetch_add(1, Ordering::SeqCst);
                                if let ExtensionEvent::TurnEnd(turn) = &event {
                                    outcomes.lock().unwrap().push(turn.outcome);
                                }
                                Ok(Some(ExtensionEventResult::TurnEnd(BoundaryResult {
                                    entries: Some(vec![draft_custom(
                                        "failure-boundary",
                                        json!(true),
                                    )]),
                                    r#continue: None,
                                })))
                            })
                        }),
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        // TS 直接覆盖 `agent.prepareRequest` 抛错。Rust 里 AgentSession 拥有 prepareRequest
        // 钩子链，因此用等价的「请求准备失败」路径：流函数返回错误 → handleRunFailure 合成
        // 失败的 assistant 消息并派发 turn_end(outcome=error)。
        harness
            .agent
            .set_stream_function(Arc::new(|_model, _context, _options| {
                Box::pin(async {
                    Err(kiana_ai::AiError::new(
                        kiana_ai::AiErrorKind::Other,
                        "request preparation failed",
                    ))
                })
            }));
        harness.set_responses(vec![faux_text_response("must not run")]);
        let _ = harness
            .session
            .prompt("start".to_string(), Default::default())
            .await;
        assert_eq!(turn_ends.load(Ordering::SeqCst), 1);
        assert_eq!(harness.faux.state().call_count(), 0);
        assert!(harness.session_entries().iter().any(|entry| match entry {
            SessionEntry::Custom(custom) => custom.custom_type == "failure-boundary",
            _ => false,
        }));
        assert_eq!(
            outcomes.lock().unwrap().as_slice(),
            &[AgentActivityOutcome::Error]
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "commits pre-settlement drafts but suppresses continuation when aborted during the hook"
#[tokio::test]
async fn commits_pre_settlement_drafts_but_suppresses_continuation_when_aborted_during_hook() {
    run(async {
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory({
                let started = Arc::clone(&started);
                let release = Arc::clone(&release);
                move |kiana| {
                    let started = Arc::clone(&started);
                    let release = Arc::clone(&release);
                    let _ = kiana.on(
                        ExtensionEventKind::AgentBeforeSettle,
                        standard(move |_event, _ctx| {
                            let started = Arc::clone(&started);
                            let release = Arc::clone(&release);
                            Box::pin(async move {
                                started.notify_one();
                                release.notified().await;
                                Ok(Some(ExtensionEventResult::AgentBeforeSettle(
                                    BoundaryResult {
                                        entries: Some(vec![draft_custom(
                                            "committed-after-abort",
                                            json!(true),
                                        )]),
                                        r#continue: Some(true),
                                    },
                                )))
                            })
                        }),
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            faux_text_response("first"),
            faux_text_response("must not run"),
        ]);
        let session = harness.session.clone();
        let prompt = tokio::task::spawn_local(async move {
            session
                .prompt("start".to_string(), Default::default())
                .await
        });
        started.notified().await;
        let session = harness.session.clone();
        let abort = tokio::task::spawn_local(async move { session.abort().await });
        release.notify_waiters();
        let _ = prompt.await.unwrap();
        abort.await.unwrap();
        assert_eq!(harness.faux.state().call_count(), 1);
        assert!(harness.session_entries().iter().any(|entry| match entry {
            SessionEntry::Custom(custom) => custom.custom_type == "committed-after-abort",
            _ => false,
        }));
        let settled = harness.events_of_type("agent_settled");
        assert_eq!(settled.len(), 1);
        assert!(matches!(
            &settled[0],
            kiana_session::AgentSessionEvent::AgentSettled { aborted: true }
        ));
        harness.cleanup();
    })
    .await;
}

/// TS: "persists custom context sent during pre-settlement before continuing"
#[tokio::test]
async fn persists_custom_context_sent_during_pre_settlement_before_continuing() {
    run(async {
        let handled = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::<String>::new()));
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory({
                let handled = Arc::clone(&handled);
                move |kiana| {
                    let handled = Arc::clone(&handled);
                    let api = Arc::clone(&kiana);
                    let _ = kiana.on(
                        ExtensionEventKind::AgentBeforeSettle,
                        standard(move |_event, _ctx| {
                            let handled = Arc::clone(&handled);
                            let api = Arc::clone(&api);
                            Box::pin(async move {
                                if handled.swap(true, Ordering::SeqCst) {
                                    return Ok(None);
                                }
                                api.send_message(kiana_ext_api::types::SendMessageOptions {
                                    message: kiana_ext_api::types::CustomMessagePayload {
                                        custom_type: "pending-boundary".to_string(),
                                        content: UserContent::Text(
                                            "persist before continue".to_string(),
                                        ),
                                        display: false,
                                        details: None,
                                    },
                                    trigger_turn: Some(false),
                                    deliver_as: None,
                                });
                                Ok(Some(ExtensionEventResult::AgentBeforeSettle(
                                    BoundaryResult {
                                        entries: None,
                                        r#continue: Some(true),
                                    },
                                )))
                            })
                        }),
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        let capture = Arc::clone(&requests);
        harness.set_responses(vec![
            faux_text_response("first"),
            FauxResponseStep::message_factory(move |input| {
                let requests = Arc::clone(&capture);
                async move {
                    requests.lock().unwrap().push(request_json(&input));
                    faux_assistant_message("second", Default::default())
                }
            }),
        ]);
        harness
            .session
            .prompt("start".to_string(), Default::default())
            .await
            .unwrap();
        assert_eq!(harness.faux.state().call_count(), 2);
        assert!(requests.lock().unwrap()[0].contains("persist before continue"));
        assert!(harness.session_entries().iter().any(|entry| match entry {
            SessionEntry::CustomMessage(message) => message.custom_type == "pending-boundary",
            _ => false,
        }));
        harness.cleanup();
    })
    .await;
}

// ============================================================================
// 回归：会话线程派发桥（评审 finding #1 / #2）
// ============================================================================

fn boundary_usage(input: u64, output: u64) -> kiana_ai::types::Usage {
    kiana_ai::types::Usage {
        input,
        output,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: None,
        reasoning: None,
        total_tokens: input + output,
        cost: kiana_ai::types::UsageCost::default(),
    }
}

/// 回归（评审 finding #1）：扩展 handler 里的真实异步等待（定时器）必须能完成。
///
/// 旧实现用临时 `LocalSet` + noop waker 在调用线程忙轮询：定时器永远不触发，20 秒后
/// panic。现在派发跑在会话线程的常驻 `LocalSet` 上，等待期间运行时可以推进。
///
/// TS 无对应用例：TS 的 `await` 天然如此；这是 Rust 桥的约束回归。
#[tokio::test]
async fn extension_handler_can_await_timers_while_the_session_waits() {
    run(async {
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(|kiana| {
                let _ = kiana.on(
                    ExtensionEventKind::ToolCall,
                    standard(move |_event, _ctx| {
                        Box::pin(async move {
                            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                            Ok(Some(ExtensionEventResult::ToolCall(
                                kiana_ext_api::events::ToolCallEventResult {
                                    block: Some(true),
                                    reason: Some("blocked after timer".to_string()),
                                    terminate: Some(false),
                                },
                            )))
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            kiana_ai::providers::faux::FauxResponseStep::Message(faux_assistant_message(
                kiana_ai::providers::faux::faux_tool_call(
                    "bash",
                    serde_json::json!({ "command": "echo hi" })
                        .as_object()
                        .cloned()
                        .map(|object| object.into_iter().collect())
                        .unwrap_or_default(),
                ),
                kiana_ai::providers::faux::FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::ToolUse),
                    ..Default::default()
                },
            )),
            faux_text_response("done"),
        ]);
        harness
            .session
            .prompt("go".to_string(), Default::default())
            .await
            .unwrap();
        let result = get_tool_result(&harness, "bash");
        assert!(result.is_error);
        assert_eq!(
            get_message_text(&kiana_agent_core::types::AgentMessage::Standard(
                kiana_ai::types::Message::ToolResult(result.clone())
            )),
            "blocked after timer"
        );
        harness.cleanup();
    })
    .await;
}

struct BoundaryInnerTool;

impl kiana_ext_api::tool_types::ToolDefinitionExecute for BoundaryInnerTool {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: serde_json::Value,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
        _ctx: Arc<dyn kiana_ext_api::types::ExtensionToolContextApi>,
    ) -> kiana_agent_core::types::BoxFuture<
        'a,
        Result<
            kiana_agent_core::types::AgentToolResult<serde_json::Value>,
            kiana_agent_core::types::AgentToolError,
        >,
    > {
        Box::pin(async move {
            Ok(kiana_agent_core::types::AgentToolResult {
                content: vec![kiana_ai::types::UserContentBlock::Text(
                    kiana_ai::types::TextContent {
                        kind_text: kiana_ai::types::TextKind::Text,
                        text: "inner-result".to_string(),
                        text_signature: None,
                    },
                )],
                details: serde_json::Value::Null,
                structured_content: None,
                usage: Some(boundary_usage(7, 3)),
                is_error: None,
                terminate: None,
            })
        })
    }
}

struct BoundaryOuterTool;

impl kiana_ext_api::tool_types::ToolDefinitionExecute for BoundaryOuterTool {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: serde_json::Value,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
        ctx: Arc<dyn kiana_ext_api::types::ExtensionToolContextApi>,
    ) -> kiana_agent_core::types::BoxFuture<
        'a,
        Result<
            kiana_agent_core::types::AgentToolResult<serde_json::Value>,
            kiana_agent_core::types::AgentToolError,
        >,
    > {
        Box::pin(async move {
            let outcome = ctx
                .execute_tool("boundary_inner", serde_json::json!({}), None)
                .await;
            let inner = outcome
                .result
                .content
                .first()
                .and_then(|block| match block {
                    kiana_ai::types::UserContentBlock::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            Ok(kiana_agent_core::types::AgentToolResult {
                content: vec![kiana_ai::types::UserContentBlock::Text(
                    kiana_ai::types::TextContent {
                        kind_text: kiana_ai::types::TextKind::Text,
                        text: format!("outer:{inner}"),
                        text_signature: None,
                    },
                )],
                details: serde_json::Value::Null,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

fn boundary_tool_definition(
    name: &str,
    execute: Arc<dyn kiana_ext_api::tool_types::ToolDefinitionExecute>,
) -> kiana_extensions::ToolDefinition {
    kiana_extensions::ToolDefinition {
        name: name.to_string(),
        label: name.to_string(),
        description: name.to_string(),
        prompt_snippet: None,
        prompt_guidelines: None,
        parameters: serde_json::json!({"type": "object", "properties": {}}),
        constrained_sampling: None,
        render_shell: None,
        prepare_arguments: None,
        output_schema: None,
        exposure: None,
        namespace: None,
        reaches_tools: None,
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute,
        render_call: None,
        render_result: None,
    }
}

/// 回归（评审 finding #2）：`ctx.executeTool` 的嵌套调用记录与用量必须落在父 toolResult 上。
///
/// TS 在 `message_start` 里原地修改同一条消息对象，agent-core 在 `message_end` 推入状态；
/// Rust 的消息是值，必须在 `message_end` 再应用替换，否则 nestedCalls/combined usage 丢失。
#[tokio::test]
async fn nested_tool_calls_and_usage_are_persisted_on_the_parent_result() {
    run(async {
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(|kiana| {
                let api = Arc::clone(&kiana);
                let _ = kiana.on(
                    ExtensionEventKind::SessionStart,
                    standard(move |_event, _ctx| {
                        let api = Arc::clone(&api);
                        Box::pin(async move {
                            api.register_tool(boundary_tool_definition(
                                "boundary_outer",
                                Arc::new(BoundaryOuterTool),
                            ));
                            api.register_tool(boundary_tool_definition(
                                "boundary_inner",
                                Arc::new(BoundaryInnerTool),
                            ));
                            Ok(None)
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        harness.session.bind_extensions(Default::default()).await;
        harness.set_responses(vec![
            kiana_ai::providers::faux::FauxResponseStep::Message(faux_assistant_message(
                kiana_ai::providers::faux::faux_tool_call(
                    "boundary_outer",
                    kiana_ai::types::JsonObject::new(),
                ),
                kiana_ai::providers::faux::FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::ToolUse),
                    ..Default::default()
                },
            )),
            faux_text_response("done"),
        ]);
        harness
            .session
            .prompt("go".to_string(), Default::default())
            .await
            .unwrap();
        let result = get_tool_result(&harness, "boundary_outer");
        assert_eq!(
            get_message_text(&kiana_agent_core::types::AgentMessage::Standard(
                kiana_ai::types::Message::ToolResult(result.clone())
            )),
            "outer:inner-result"
        );
        let nested = result.nested_calls.expect("nested calls persisted");
        assert!(nested.complete);
        assert_eq!(nested.calls.len(), 1);
        assert_eq!(nested.calls[0]["name"], serde_json::json!("boundary_inner"));
        assert_eq!(nested.calls[0]["status"], serde_json::json!("ok"));
        assert_eq!(result.usage, Some(boundary_usage(7, 3)));
        harness.cleanup();
    })
    .await;
}

/// 回归（评审 finding #9）：扩展 handler 内部派发的 fire-and-forget 扩展事件必须送达。
///
/// 旧实现把 `spawn_local` 调度到当次派发的临时 `LocalSet` 上，外层派发结束即被丢弃；
/// 现在它们落在会话线程的常驻派发表上。
#[tokio::test]
async fn extension_events_emitted_from_inside_a_handler_are_delivered() {
    run(async {
        let observed: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory({
                let observed = Arc::clone(&observed);
                move |kiana| {
                    let observed = Arc::clone(&observed);
                    let api = Arc::clone(&kiana);
                    let _ = kiana.on(
                        ExtensionEventKind::SessionInfoChanged,
                        standard(move |event, _ctx| {
                            let observed = Arc::clone(&observed);
                            Box::pin(async move {
                                let ExtensionEvent::Session(SessionEvent::InfoChanged(event)) =
                                    event
                                else {
                                    return Ok(None);
                                };
                                observed.lock().unwrap().push(event.name);
                                Ok(None)
                            })
                        }),
                    );
                    let _ = kiana.on(
                        ExtensionEventKind::ToolCall,
                        standard(move |_event, _ctx| {
                            let api = Arc::clone(&api);
                            Box::pin(async move {
                                api.set_session_name("renamed-from-handler");
                                Ok(None)
                            })
                        }),
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            kiana_ai::providers::faux::FauxResponseStep::Message(faux_assistant_message(
                kiana_ai::providers::faux::faux_tool_call(
                    "bash",
                    serde_json::json!({ "command": "echo hi" })
                        .as_object()
                        .cloned()
                        .map(|object| object.into_iter().collect())
                        .unwrap_or_default(),
                ),
                kiana_ai::providers::faux::FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::ToolUse),
                    ..Default::default()
                },
            )),
            faux_text_response("done"),
        ]);
        harness
            .session
            .prompt("go".to_string(), Default::default())
            .await
            .unwrap();
        // 扩展事件由常驻派发表异步送达；等待一轮任务调度。
        for _ in 0..2000 {
            if observed
                .lock()
                .unwrap()
                .iter()
                .any(|name| name.as_deref() == Some("renamed-from-handler"))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        assert_eq!(
            observed.lock().unwrap().clone(),
            vec![Some("renamed-from-handler".to_string())]
        );
        harness.cleanup();
    })
    .await;
}

// ============================================================================
// 边界压缩链（评审：invariant #2 的 `_checkCompaction` 用例）
// ============================================================================

fn boundary_token_usage(input: u64, output: u64) -> kiana_ai::types::Usage {
    kiana_ai::types::Usage {
        input,
        output,
        total_tokens: input + output,
        ..Default::default()
    }
}

fn replace_assistant_usage(
    message: kiana_agent_core::types::AgentMessage,
    input: u64,
    output: u64,
) -> kiana_agent_core::types::AgentMessage {
    match message {
        kiana_agent_core::types::AgentMessage::Standard(kiana_ai::types::Message::Assistant(
            mut assistant,
        )) => {
            assistant.usage = boundary_token_usage(input, output);
            kiana_agent_core::types::AgentMessage::Standard(kiana_ai::types::Message::Assistant(
                assistant,
            ))
        }
        other => other,
    }
}

/// TS: "does not compact from usage belonging to a boundary-omitted assistant"
#[tokio::test]
async fn does_not_compact_from_usage_belonging_to_a_boundary_omitted_assistant() {
    run(async {
        let handled = Arc::new(AtomicBool::new(false));
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(10_000),
                max_tokens: Some(100),
                ..Default::default()
            }]),
            settings: Some(settings_from_json(serde_json::json!({
                "compaction": { "enabled": true, "keepRecentTokens": 1, "reserveTokens": 300 }
            }))),
            extension_factories: vec![factory({
                let handled = Arc::clone(&handled);
                move |kiana| {
                    let _ = kiana.on(
                        ExtensionEventKind::MessageEnd,
                        standard(move |event, _ctx| {
                            Box::pin(async move {
                                let ExtensionEvent::MessageEnd(event) = event else {
                                    return Ok(None);
                                };
                                if event.message.role() != "assistant" {
                                    return Ok(None);
                                }
                                Ok(Some(ExtensionEventResult::MessageEnd(
                                    kiana_ext_api::events::MessageEndEventResult {
                                        message: Some(replace_assistant_usage(
                                            event.message,
                                            9_800,
                                            1,
                                        )),
                                    },
                                )))
                            })
                        }),
                    );
                    let handled = Arc::clone(&handled);
                    let _ = kiana.on(
                        ExtensionEventKind::TurnEnd,
                        standard(move |event, _ctx| {
                            let handled = Arc::clone(&handled);
                            Box::pin(async move {
                                let ExtensionEvent::TurnEnd(turn) = event else {
                                    return Ok(None);
                                };
                                if handled.swap(true, Ordering::SeqCst) {
                                    return Ok(None);
                                }
                                turn_end_result(
                                    vec![draft_context_edit(&turn.message_entry_id, None)],
                                    false,
                                )
                            })
                        }),
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![faux_text_response("short response")]);
        harness
            .session
            .prompt("small prompt".to_string(), Default::default())
            .await
            .unwrap();
        assert!(harness.events_of_type("compaction_start").is_empty());
        let tokens = harness
            .session
            .get_context_usage()
            .and_then(|usage| usage.tokens)
            .unwrap_or(0);
        assert!(tokens < 2_000, "unexpected context usage: {tokens}");
        harness.cleanup();
    })
    .await;
}

/// TS: "does not trigger successful-response overflow from usage captured before a boundary edit"
#[tokio::test]
async fn does_not_trigger_successful_response_overflow_from_usage_captured_before_a_boundary_edit()
{
    run(async {
        let handled = Arc::new(AtomicBool::new(false));
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(5_000),
                max_tokens: Some(100),
                ..Default::default()
            }]),
            settings: Some(settings_from_json(serde_json::json!({
                "compaction": { "enabled": true, "keepRecentTokens": 1, "reserveTokens": 0 }
            }))),
            extension_factories: vec![factory({
                let handled = Arc::clone(&handled);
                move |kiana| {
                    let _ = kiana.on(
                        ExtensionEventKind::MessageEnd,
                        standard(move |event, _ctx| {
                            Box::pin(async move {
                                let ExtensionEvent::MessageEnd(event) = event else {
                                    return Ok(None);
                                };
                                if event.message.role() != "assistant" {
                                    return Ok(None);
                                }
                                Ok(Some(ExtensionEventResult::MessageEnd(
                                    kiana_ext_api::events::MessageEndEventResult {
                                        message: Some(replace_assistant_usage(
                                            event.message,
                                            5_100,
                                            1,
                                        )),
                                    },
                                )))
                            })
                        }),
                    );
                    let handled = Arc::clone(&handled);
                    let _ = kiana.on(
                        ExtensionEventKind::TurnEnd,
                        standard(move |event, ctx| {
                            let handled = Arc::clone(&handled);
                            Box::pin(async move {
                                if !matches!(event, ExtensionEvent::TurnEnd(_)) {
                                    return Ok(None);
                                }
                                if handled.swap(true, Ordering::SeqCst) {
                                    return Ok(None);
                                }
                                let user = ctx
                                    .session_manager()
                                    .get_branch(None)
                                    .into_iter()
                                    .rev()
                                    .find(|entry| match entry {
                                        SessionEntry::Message(message) => {
                                            message.message.role() == "user"
                                        }
                                        _ => false,
                                    });
                                let Some(user) = user else {
                                    return Ok(None);
                                };
                                turn_end_result(vec![draft_context_edit(user.id(), None)], false)
                            })
                        }),
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![faux_text_response("done")]);
        harness
            .session
            .prompt(
                "large input that is later omitted".to_string(),
                Default::default(),
            )
            .await
            .unwrap();
        assert!(harness.events_of_type("compaction_start").is_empty());
        let tokens = harness
            .session
            .get_context_usage()
            .and_then(|usage| usage.tokens)
            .unwrap_or(0);
        assert!(tokens < 2_000, "unexpected context usage: {tokens}");
        harness.cleanup();
    })
    .await;
}

/// TS: "does not treat retained pre-compaction assistant usage as post-compaction usage"
#[tokio::test]
async fn does_not_treat_retained_pre_compaction_assistant_usage_as_post_compaction_usage() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        let retained = {
            let model = harness.get_model(None).expect("model");
            let mut message = faux_assistant_message("retained", Default::default());
            message.api = model.api.clone();
            message.provider = model.provider.clone();
            message.model = model.id.clone();
            message.usage = boundary_token_usage(10_000, 1);
            kiana_agent_core::types::AgentMessage::Standard(kiana_ai::types::Message::Assistant(
                message,
            ))
        };
        let retained_id = harness
            .session
            .with_session_manager_mut(|manager| manager.append_message(retained))
            .expect("append");
        harness
            .session
            .with_session_manager_mut(|manager| {
                manager.append_compaction("summary", Some(&retained_id), 10_001.0, None, None, None)
            })
            .expect("append compaction");
        harness.session.refresh_context();
        assert_eq!(
            harness
                .session
                .get_context_usage()
                .and_then(|usage| usage.tokens),
            None
        );
        harness.cleanup();
    })
    .await;
}
