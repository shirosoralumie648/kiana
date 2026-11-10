//! `test/suite/agent-session-boundaries.test.ts` 中剩余验收用例的 Rust 移植。
//!
//! 覆盖：边界替换/输入的阈值压缩保持、pre-settlement follow-up、system-only 草稿、
//! post-edit usage、长度恢复（截断工具、重置、独立 follow-up、失败不重试）、
//! 非可重试错误的 retry 记账、recoverable replacement 省略、显式 overflow 恢复、
//! follow-up 与错误重试的先后、耗尽重试标记 final。

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use common::*;
use kiana_agent_core::types::{AgentMessage, AgentTool, AgentToolError, AgentToolExecute};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message,
};
use kiana_ai::types::{Message, StopReason, Usage, UsageCost, UserContent, UserContentBlock};
use kiana_ext_api::events::{
    AgentActivityOutcome, BoundaryResult, CompactionEntryDraft, ContextEditEntryDraft,
    ContextEditEntryDraftType, CustomEntryDraft, CustomEntryDraftType, CustomMessageEntryDraft,
    CustomMessageEntryDraftType, ExtensionEvent, ExtensionEventKind, ExtensionEventResult,
    SessionBeforeCompactResult, SessionBoundaryDraft,
};
use kiana_ext_api::types::{SendUserMessageDeliverAs, SendUserMessageOptions};
use kiana_session::agent_session::{AgentSessionEvent, PromptOptions};
use kiana_session_core::session_manager::{
    ContextEditReplacement, SessionEntry, SessionMessageType,
};
use serde_json::json;

// ============================================================================
// 辅助
// ============================================================================

fn draft_context_edit(target_id: &str, replacement: Option<&str>) -> SessionBoundaryDraft {
    SessionBoundaryDraft::ContextEdit(ContextEditEntryDraft {
        event_type: ContextEditEntryDraftType::ContextEdit,
        target_id: target_id.to_string(),
        replacement: replacement.map(|content| ContextEditReplacement {
            content: json!(content),
        }),
    })
}

fn draft_context_edit_json(
    target_id: &str,
    replacement: Option<serde_json::Value>,
) -> SessionBoundaryDraft {
    SessionBoundaryDraft::ContextEdit(ContextEditEntryDraft {
        event_type: ContextEditEntryDraftType::ContextEdit,
        target_id: target_id.to_string(),
        replacement: replacement.map(|content| ContextEditReplacement { content }),
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

fn turn_end_result(entries: Vec<SessionBoundaryDraft>, r#continue: bool) -> HandlerResult {
    Ok(Some(ExtensionEventResult::TurnEnd(BoundaryResult {
        entries: Some(entries),
        r#continue: Some(r#continue),
    })))
}

fn before_settle_result(entries: Vec<SessionBoundaryDraft>, r#continue: bool) -> HandlerResult {
    Ok(Some(ExtensionEventResult::AgentBeforeSettle(
        BoundaryResult {
            entries: Some(entries),
            r#continue: Some(r#continue),
        },
    )))
}

fn capturing_response(requests: Arc<Mutex<Vec<String>>>, text: &str) -> FauxResponseStep {
    let text = text.to_string();
    FauxResponseStep::message_factory(move |input| {
        let requests = Arc::clone(&requests);
        let text = text.clone();
        async move {
            requests.lock().unwrap().push(
                serde_json::to_string(&input.context.as_context().messages).unwrap_or_default(),
            );
            faux_assistant_message(text, Default::default())
        }
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn assistant_with_usage(
    text: &str,
    stop_reason: StopReason,
    input: u64,
    output: u64,
    timestamp: i64,
) -> kiana_ai::types::AssistantMessage {
    let mut assistant = faux_assistant_message(
        text,
        FauxAssistantMessageOptions {
            stop_reason: Some(stop_reason),
            timestamp: Some(timestamp),
            ..Default::default()
        },
    );
    assistant.usage = Usage {
        input,
        output,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: None,
        reasoning: None,
        total_tokens: input + output,
        cost: UsageCost::default(),
    };
    assistant
}

fn compaction_factory(summary: &'static str) -> kiana_ext_api::types::ExtensionFactory {
    factory(move |kiana| {
        let _ = kiana.on(
            ExtensionEventKind::SessionBeforeCompact,
            standard(move |event, _ctx| {
                Box::pin(async move {
                    let ExtensionEvent::Session(
                        kiana_ext_api::events::SessionEvent::BeforeCompact(before),
                    ) = event
                    else {
                        return Ok(None);
                    };
                    Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                        SessionBeforeCompactResult {
                            cancel: None,
                            compaction: Some(kiana_compaction::compaction::CompactionResult {
                                summary: summary.to_string(),
                                first_kept_entry_id: before.preparation.first_kept_entry_id.clone(),
                                tokens_before: before.preparation.tokens_before,
                                estimated_tokens_after: None,
                                usage: None,
                                details: None,
                            }),
                        },
                    )))
                })
            }),
        );
    })
}

fn cancel_compaction_factory() -> kiana_ext_api::types::ExtensionFactory {
    factory(|kiana| {
        let _ = kiana.on(
            ExtensionEventKind::SessionBeforeCompact,
            standard(|_event, _ctx| {
                Box::pin(async {
                    Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                        SessionBeforeCompactResult {
                            cancel: Some(true),
                            compaction: None,
                        },
                    )))
                })
            }),
        );
    })
}

struct NoopTool;

impl AgentToolExecute for NoopTool {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: serde_json::Value,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
    ) -> kiana_agent_core::types::BoxFuture<
        'a,
        Result<kiana_agent_core::types::AgentToolResult<serde_json::Value>, AgentToolError>,
    > {
        Box::pin(async {
            Ok(kiana_agent_core::types::AgentToolResult {
                content: vec![UserContentBlock::Text(kiana_ai::types::TextContent {
                    kind_text: kiana_ai::types::TextKind::Text,
                    text: "done".to_string(),
                    text_signature: None,
                })],
                details: serde_json::Value::Null,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

fn noop_tool() -> AgentTool {
    AgentTool {
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
    }
}

fn context_edits(harness: &Harness) -> Vec<kiana_session_core::session_manager::ContextEditEntry> {
    harness
        .session_manager()
        .get_entries()
        .into_iter()
        .filter_map(|entry| match entry {
            SessionEntry::ContextEdit(edit) => Some(edit),
            _ => None,
        })
        .collect()
}

// ============================================================================
// 用例
// ============================================================================

/// TS: "keeps a boundary replacement verbatim through threshold compaction"
#[tokio::test]
async fn keeps_boundary_replacement_verbatim_through_threshold_compaction() {
    run(async {
        let handled = Arc::new(AtomicBool::new(false));
        let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let instruction = "EXACT-REPLACEMENT-INSTRUCTION ".repeat(100);
        let instruction_for_handler = instruction.clone();
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(2_000),
                max_tokens: Some(100),
                ..Default::default()
            }]),
            settings: Some(settings_from_json(json!({
                "compaction": { "enabled": true, "keepRecentTokens": 1, "reserveTokens": 0 }
            }))),
            extension_factories: vec![
                factory(move |kiana| {
                    let handled = Arc::clone(&handled);
                    let instruction = instruction_for_handler.clone();
                    let _ = kiana.on(
                        ExtensionEventKind::TurnEnd,
                        standard(move |event, ctx| {
                            let handled = Arc::clone(&handled);
                            let instruction = instruction.clone();
                            Box::pin(async move {
                                let ExtensionEvent::TurnEnd(turn) = event else {
                                    return Ok(None);
                                };
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
                                    })
                                    .map(|entry| entry.id().to_string())
                                    .ok_or("missing user entry")?;
                                turn_end_result(
                                    vec![
                                        draft_context_edit(&user, Some(&instruction)),
                                        draft_context_edit(&turn.message_entry_id, None),
                                        draft_custom("bookkeeping", json!({"source": "test"})),
                                    ],
                                    true,
                                )
                            })
                        }),
                    );
                }),
                compaction_factory("older history summary"),
            ],
            ..Default::default()
        })
        .await;
        harness.append_session_message(user_msg("older input"));
        harness.append_session_message(assistant_msg("older answer"));
        harness.set_responses(vec![
            faux_text_response("answered original input"),
            capturing_response(Arc::clone(&requests), "answered replacement"),
        ]);

        harness
            .session
            .prompt("original input".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert!(!harness.events_of_type("compaction_start").is_empty());
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("EXACT-REPLACEMENT-INSTRUCTION"));
        harness.cleanup();
    })
    .await;
}

/// TS: "keeps boundary input verbatim through threshold compaction when metadata follows it"
#[tokio::test]
async fn keeps_boundary_input_verbatim_when_metadata_follows_it() {
    run(async {
        let handled = Arc::new(AtomicBool::new(false));
        let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let instruction = "EXACT-UNSENT-INSTRUCTION ".repeat(100);
        let instruction_for_handler = instruction.clone();
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(2_000),
                max_tokens: Some(100),
                ..Default::default()
            }]),
            settings: Some(settings_from_json(json!({
                "compaction": { "enabled": true, "keepRecentTokens": 1, "reserveTokens": 0 }
            }))),
            extension_factories: vec![
                factory(move |kiana| {
                    let handled = Arc::clone(&handled);
                    let instruction = instruction_for_handler.clone();
                    let _ = kiana.on(
                        ExtensionEventKind::TurnEnd,
                        standard(move |_event, _ctx| {
                            let handled = Arc::clone(&handled);
                            let instruction = instruction.clone();
                            Box::pin(async move {
                                if handled.swap(true, Ordering::SeqCst) {
                                    return Ok(None);
                                }
                                turn_end_result(
                                    vec![
                                        draft_custom_message("next-work", &instruction, false),
                                        draft_custom("bookkeeping", json!({"source": "test"})),
                                    ],
                                    true,
                                )
                            })
                        }),
                    );
                }),
                compaction_factory("older history summary"),
            ],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            faux_text_response("first"),
            capturing_response(Arc::clone(&requests), "second"),
        ]);

        harness
            .session
            .prompt("old input ".repeat(500), PromptOptions::default())
            .await
            .unwrap();

        assert!(!harness.events_of_type("compaction_start").is_empty());
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("EXACT-UNSENT-INSTRUCTION"));
        harness.cleanup();
    })
    .await;
}

/// TS: "keeps a pre-settlement follow-up deferred until the explicit continuation would stop"
#[tokio::test]
async fn keeps_pre_settlement_follow_up_deferred() {
    run(async {
        let handled = Arc::new(AtomicBool::new(false));
        let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(move |kiana| {
                let handled = Arc::clone(&handled);
                let api = Arc::clone(&kiana);
                let _ = kiana.on(
                    ExtensionEventKind::AgentBeforeSettle,
                    standard(move |_event, _ctx| {
                        let handled = Arc::clone(&handled);
                        let kiana = Arc::clone(&api);
                        Box::pin(async move {
                            if handled.swap(true, Ordering::SeqCst) {
                                return Ok(None);
                            }
                            kiana.send_user_message(
                                UserContent::Text("queued follow-up".to_string()),
                                Some(SendUserMessageOptions {
                                    deliver_as: Some(SendUserMessageDeliverAs::FollowUp),
                                    expand_prompt_templates: None,
                                }),
                            );
                            before_settle_result(
                                vec![draft_custom_message("boundary", "boundary context", false)],
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
            faux_text_response("first"),
            capturing_response(Arc::clone(&requests), "second"),
            capturing_response(Arc::clone(&requests), "follow-up response"),
        ]);

        harness
            .session
            .prompt("start".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(harness.faux.state().call_count(), 3);
        let requests = requests.lock().unwrap();
        assert!(requests[0].contains("boundary context"));
        assert!(!requests[0].contains("queued follow-up"));
        assert!(requests[1].contains("queued follow-up"));
        harness.cleanup();
    })
    .await;
}

/// TS: "does not consume queued input when pre-settlement drafts leave system-only context"
#[tokio::test]
async fn does_not_consume_queued_input_when_drafts_leave_system_only_context() {
    run(async {
        let handled = Arc::new(AtomicBool::new(false));
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(move |kiana| {
                let handled = Arc::clone(&handled);
                let api = Arc::clone(&kiana);
                let _ = kiana.on(
                    ExtensionEventKind::AgentBeforeSettle,
                    standard(move |_event, ctx| {
                        let handled = Arc::clone(&handled);
                        let kiana = Arc::clone(&api);
                        Box::pin(async move {
                            if handled.swap(true, Ordering::SeqCst) {
                                return Ok(None);
                            }
                            kiana.send_user_message(
                                UserContent::Text("still queued".to_string()),
                                Some(SendUserMessageOptions {
                                    deliver_as: Some(SendUserMessageDeliverAs::FollowUp),
                                    expand_prompt_templates: None,
                                }),
                            );
                            let targets: Vec<String> = ctx
                                .session_manager()
                                .get_branch(None)
                                .into_iter()
                                .filter_map(|entry| match &entry {
                                    SessionEntry::Message(message)
                                        if matches!(
                                            message.message.role(),
                                            "user" | "assistant" | "toolResult"
                                        ) =>
                                    {
                                        Some(entry.id().to_string())
                                    }
                                    _ => None,
                                })
                                .collect();
                            before_settle_result(
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
            faux_text_response("first"),
            faux_text_response("must not run"),
        ]);

        harness
            .session
            .prompt("start".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(harness.faux.state().call_count(), 1);
        assert_eq!(harness.session.pending_message_count(), 1);
        harness.cleanup();
    })
    .await;
}

/// TS: "does not trigger threshold compaction from post-edit usage captured before a later compaction"
#[tokio::test]
async fn does_not_trigger_threshold_compaction_from_post_edit_usage() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(10_000),
                max_tokens: Some(100),
                ..Default::default()
            }]),
            settings: Some(settings_from_json(json!({
                "compaction": { "enabled": true, "keepRecentTokens": 1, "reserveTokens": 0 }
            }))),
            ..Default::default()
        })
        .await;
        let user_id = harness.append_session_message(user_msg("small input"));
        harness
            .session
            .with_session_manager_mut(|manager| {
                manager.append_context_edit(
                    &user_id,
                    Some(ContextEditReplacement {
                        content: json!("edited input"),
                    }),
                )
            })
            .expect("context edit");
        let response = assistant_with_usage("answer", StopReason::Stop, 50_000, 1, now_ms() - 2);
        harness.append_session_message(AgentMessage::Standard(Message::Assistant(response)));
        harness
            .session
            .with_session_manager_mut(|manager| {
                manager.append_compaction(
                    "small summary",
                    Some(&user_id),
                    50_001.0,
                    None,
                    None,
                    None,
                )
            })
            .expect("compaction");
        harness.session.refresh_context();

        let error = assistant_with_usage("", StopReason::Error, 0, 0, now_ms() + 1_000);
        let mut error = error;
        error.error_message = Some("invalid_api_key".to_string());

        harness.session.check_compaction_for_test(&error).await;

        assert!(harness.events_of_type("compaction_start").is_empty());
        harness.cleanup();
    })
    .await;
}

/// TS: "keeps truncated tool attempts in context for the natural next turn"
#[tokio::test]
async fn keeps_truncated_tool_attempts_in_context() {
    run(async {
        let executed = Arc::new(AtomicBool::new(false));
        struct TruncatedTool {
            executed: Arc<AtomicBool>,
        }
        impl AgentToolExecute for TruncatedTool {
            fn execute<'a>(
                &'a self,
                _tool_call_id: &'a str,
                _params: serde_json::Value,
                _signal: Option<&'a tokio_util::sync::CancellationToken>,
                _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
            ) -> kiana_agent_core::types::BoxFuture<
                'a,
                Result<kiana_agent_core::types::AgentToolResult<serde_json::Value>, AgentToolError>,
            > {
                self.executed.store(true, Ordering::SeqCst);
                Box::pin(async {
                    Ok(kiana_agent_core::types::AgentToolResult {
                        content: vec![UserContentBlock::Text(kiana_ai::types::TextContent {
                            kind_text: kiana_ai::types::TextKind::Text,
                            text: "executed".to_string(),
                            text_signature: None,
                        })],
                        details: serde_json::Value::Null,
                        structured_content: None,
                        usage: None,
                        is_error: None,
                        terminate: None,
                    })
                })
            }
        }
        let tool = AgentTool {
            name: "unsafe_truncated_tool".to_string(),
            label: "Unsafe truncated tool".to_string(),
            description: "Must not execute from a length response".to_string(),
            parameters: json!({"type": "object", "properties": {"value": {"type": "string"}}}),
            constrained_sampling: None,
            output_schema: None,
            prepare_arguments: None,
            execution_mode: None,
            replay: None,
            execute: Arc::new(TruncatedTool {
                executed: Arc::clone(&executed),
            }),
        };
        let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![tool]),
            ..Default::default()
        })
        .await;
        let mut arguments = kiana_ai::types::JsonObject::new();
        arguments.insert("value".to_string(), json!("partial"));
        let call = kiana_ai::providers::faux::faux_tool_call("unsafe_truncated_tool", arguments);
        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                call,
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::Length),
                    ..Default::default()
                },
            )),
            capturing_response(Arc::clone(&requests), "completed natural continuation"),
        ]);

        harness
            .session
            .prompt("start".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert!(!executed.load(Ordering::SeqCst));
        assert_eq!(harness.faux.state().call_count(), 2);
        assert!(requests.lock().unwrap()[0].contains("may be truncated"));
        assert!(context_edits(&harness).is_empty());
        harness.cleanup();
    })
    .await;
}

/// TS: "resets length recovery after a successful intermediate assistant turn"
#[tokio::test]
async fn resets_length_recovery_after_successful_intermediate_turn() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(1000),
                max_tokens: Some(100),
                ..Default::default()
            }]),
            settings: Some(settings_from_json(json!({
                "compaction": { "keepRecentTokens": 1, "reserveTokens": 0 }
            }))),
            tools: Some(vec![noop_tool()]),
            extension_factories: vec![compaction_factory("recovered input")],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                "first partial",
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::Length),
                    ..Default::default()
                },
            )),
            FauxResponseStep::Message(faux_assistant_message(
                kiana_ai::providers::faux::faux_tool_call("noop", indexmap::IndexMap::new()),
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::ToolUse),
                    ..Default::default()
                },
            )),
            FauxResponseStep::Message(faux_assistant_message(
                "second partial",
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::Length),
                    timestamp: Some(now_ms() + 1_000),
                    ..Default::default()
                },
            )),
            FauxResponseStep::Message(faux_assistant_message(
                "completed second recovery",
                FauxAssistantMessageOptions {
                    timestamp: Some(now_ms() + 2_000),
                    ..Default::default()
                },
            )),
        ]);

        harness
            .session
            .prompt("x".repeat(5000), PromptOptions::default())
            .await
            .unwrap();

        let omitted: Vec<String> = context_edits(&harness)
            .into_iter()
            .map(|edit| edit.target_id)
            .collect();
        let length_ids: Vec<String> = harness
            .session_manager()
            .get_entries()
            .into_iter()
            .filter_map(|entry| match &entry {
                SessionEntry::Message(message)
                    if message.message.role() == "assistant"
                        && matches!(
                            &message.message,
                            AgentMessage::Standard(Message::Assistant(assistant))
                                if assistant.stop_reason == StopReason::Length
                        ) =>
                {
                    Some(entry.id().to_string())
                }
                _ => None,
            })
            .collect();
        assert_eq!(length_ids.len(), 2);
        for id in &length_ids {
            assert!(omitted.contains(id), "missing omission for {id}");
        }
        assert_eq!(harness.faux.state().call_count(), 3);
        harness.cleanup();
    })
    .await;
}

/// TS: "gives a distinct queued follow-up its own length-recovery budget"
#[tokio::test]
async fn gives_distinct_queued_follow_up_its_own_length_recovery_budget() {
    run(async {
        let queued = Arc::new(AtomicBool::new(false));
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(1000),
                max_tokens: Some(100),
                ..Default::default()
            }]),
            settings: Some(settings_from_json(json!({
                "compaction": { "keepRecentTokens": 1, "reserveTokens": 0 }
            }))),
            extension_factories: vec![
                compaction_factory("recovered input"),
                factory(move |kiana| {
                    let queued = Arc::clone(&queued);
                    let api = Arc::clone(&kiana);
                    let _ = kiana.on(
                        ExtensionEventKind::AgentEnd,
                        standard(move |event, _ctx| {
                            let queued = Arc::clone(&queued);
                            let kiana = Arc::clone(&api);
                            Box::pin(async move {
                                let ExtensionEvent::AgentEnd(end) = event else {
                                    return Ok(None);
                                };
                                if queued.load(Ordering::SeqCst)
                                    || !end.messages.iter().any(|message| {
                                        get_message_text(message) == "first recovered"
                                    })
                                {
                                    return Ok(None);
                                }
                                queued.store(true, Ordering::SeqCst);
                                kiana.send_user_message(
                                    UserContent::Text("distinct follow-up".to_string()),
                                    Some(SendUserMessageOptions {
                                        deliver_as: Some(SendUserMessageDeliverAs::FollowUp),
                                        expand_prompt_templates: None,
                                    }),
                                );
                                Ok(None)
                            })
                        }),
                    );
                }),
            ],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                "first partial",
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::Length),
                    ..Default::default()
                },
            )),
            faux_text_response("first recovered"),
            FauxResponseStep::Message(faux_assistant_message(
                "follow-up partial",
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::Length),
                    timestamp: Some(now_ms() + 1_000),
                    ..Default::default()
                },
            )),
            FauxResponseStep::Message(faux_assistant_message(
                "follow-up recovered",
                FauxAssistantMessageOptions {
                    timestamp: Some(now_ms() + 2_000),
                    ..Default::default()
                },
            )),
        ]);

        harness
            .session
            .prompt("x".repeat(5000), PromptOptions::default())
            .await
            .unwrap();

        let length_ids: Vec<String> = harness
            .session_manager()
            .get_entries()
            .into_iter()
            .filter_map(|entry| match &entry {
                SessionEntry::Message(message)
                    if message.message.role() == "assistant"
                        && matches!(
                            &message.message,
                            AgentMessage::Standard(Message::Assistant(assistant))
                                if assistant.stop_reason == StopReason::Length
                        ) =>
                {
                    Some(entry.id().to_string())
                }
                _ => None,
            })
            .collect();
        let omitted: Vec<String> = context_edits(&harness)
            .into_iter()
            .map(|edit| edit.target_id)
            .collect();
        assert_eq!(length_ids.len(), 2);
        for id in &length_ids {
            assert!(omitted.contains(id), "missing omission for {id}");
        }
        assert_eq!(harness.faux.state().call_count(), 4);
        harness.cleanup();
    })
    .await;
}

/// TS: "finishes retry bookkeeping when a retry receives a nonretryable error"
#[tokio::test]
async fn finishes_retry_bookkeeping_for_nonretryable_error() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_from_json(json!({
                "retry": { "enabled": true, "maxRetries": 2, "baseDelayMs": 1 }
            }))),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                "",
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::Error),
                    error_message: Some("overloaded_error".to_string()),
                    ..Default::default()
                },
            )),
            FauxResponseStep::Message(faux_assistant_message(
                "",
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::Error),
                    error_message: Some("invalid_api_key".to_string()),
                    ..Default::default()
                },
            )),
        ]);

        harness
            .session
            .prompt("start".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(harness.faux.state().call_count(), 2);
        let end = harness
            .events_of_type("auto_retry_end")
            .into_iter()
            .next_back()
            .expect("auto_retry_end");
        match end {
            AgentSessionEvent::AutoRetryEnd {
                success,
                attempt,
                final_error,
            } => {
                assert!(!success);
                assert_eq!(attempt, 1);
                assert_eq!(final_error.as_deref(), Some("invalid_api_key"));
            }
            other => panic!("unexpected event: {other:?}"),
        }
        harness.cleanup();
    })
    .await;
}

/// TS: "omits a recoverable projected replacement by its source entry ID"
#[tokio::test]
async fn omits_recoverable_projected_replacement_by_source_entry_id() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(1_000),
                max_tokens: Some(100),
                ..Default::default()
            }]),
            settings: Some(settings_from_json(json!({
                "compaction": { "enabled": true, "keepRecentTokens": 1, "reserveTokens": 0 }
            }))),
            extension_factories: vec![cancel_compaction_factory()],
            ..Default::default()
        })
        .await;
        harness.append_session_message(user_msg(&"x".repeat(5_000)));
        let partial =
            assistant_with_usage("original partial", StopReason::Length, 0, 0, now_ms() - 1);
        let partial_id =
            harness.append_session_message(AgentMessage::Standard(Message::Assistant(partial)));
        harness
            .session
            .with_session_manager_mut(|manager| {
                manager.append_context_edit(
                    &partial_id,
                    Some(ContextEditReplacement {
                        content: json!([{"type": "text", "text": "edited partial"}]),
                    }),
                )
            })
            .expect("context edit");
        harness.session.refresh_context();
        harness.set_responses(vec![faux_text_response("new answer")]);

        harness
            .session
            .prompt("next prompt".to_string(), PromptOptions::default())
            .await
            .unwrap();

        let edits = context_edits(&harness);
        let last = edits
            .iter()
            .rev()
            .find(|edit| edit.target_id == partial_id)
            .expect("edit for partial");
        assert!(last.replacement.is_none());
        assert!(
            !harness
                .session_manager()
                .build_session_projection()
                .messages
                .iter()
                .any(|message| get_message_text(message) == "edited partial")
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "recovers an explicit overflow error after a retained boundary replacement"
#[tokio::test]
async fn recovers_explicit_overflow_error_after_retained_boundary_replacement() {
    run(async {
        let replaced = Arc::new(AtomicBool::new(false));
        let overflow_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let overflow_for_factory = Arc::clone(&overflow_id);
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(1_000),
                max_tokens: Some(100),
                ..Default::default()
            }]),
            settings: Some(settings_from_json(json!({
                "compaction": { "enabled": true, "keepRecentTokens": 1, "reserveTokens": 0 }
            }))),
            extension_factories: vec![
                factory(move |kiana| {
                    let replaced = Arc::clone(&replaced);
                    let overflow_id = Arc::clone(&overflow_for_factory);
                    let _ = kiana.on(
                        ExtensionEventKind::TurnEnd,
                        standard(move |event, _ctx| {
                            let replaced = Arc::clone(&replaced);
                            let overflow_id = Arc::clone(&overflow_id);
                            Box::pin(async move {
                                let ExtensionEvent::TurnEnd(turn) = event else {
                                    return Ok(None);
                                };
                                if replaced.swap(true, Ordering::SeqCst)
                                    || turn.outcome != AgentActivityOutcome::Error
                                {
                                    return Ok(None);
                                }
                                *overflow_id.lock().unwrap() = Some(turn.message_entry_id.clone());
                                turn_end_result(
                                    vec![draft_context_edit_json(
                                        &turn.message_entry_id,
                                        Some(json!([{"type": "text", "text": "retained error"}])),
                                    )],
                                    false,
                                )
                            })
                        }),
                    );
                }),
                compaction_factory("recovered overflow"),
            ],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                "retained error",
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::Error),
                    error_message: Some("prompt is too long".to_string()),
                    ..Default::default()
                },
            )),
            faux_text_response("recovered"),
        ]);

        harness
            .session
            .prompt("x".repeat(5000), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(harness.faux.state().call_count(), 2);
        let overflow_id = overflow_id
            .lock()
            .unwrap()
            .clone()
            .expect("overflow message id");
        let last = context_edits(&harness)
            .into_iter()
            .rev()
            .find(|edit| edit.target_id == overflow_id)
            .expect("edit for overflow");
        assert!(last.replacement.is_none());
        harness.cleanup();
    })
    .await;
}

/// TS: "keeps follow-up work behind an automatic error retry"
#[tokio::test]
async fn keeps_follow_up_work_behind_automatic_error_retry() {
    run(async {
        let queued = Arc::new(AtomicBool::new(false));
        let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let lifecycle: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_from_json(json!({
                "retry": { "enabled": true, "maxRetries": 2, "baseDelayMs": 1 }
            }))),
            extension_factories: vec![factory(move |kiana| {
                let queued = Arc::clone(&queued);
                let api = Arc::clone(&kiana);
                let _ = kiana.on(
                    ExtensionEventKind::TurnEnd,
                    standard(move |event, _ctx| {
                        let queued = Arc::clone(&queued);
                        let kiana = Arc::clone(&api);
                        Box::pin(async move {
                            let ExtensionEvent::TurnEnd(turn) = event else {
                                return Ok(None);
                            };
                            if queued.swap(true, Ordering::SeqCst)
                                || turn.outcome != AgentActivityOutcome::Error
                            {
                                return Ok(None);
                            }
                            kiana.send_user_message(
                                UserContent::Text("queued follow-up".to_string()),
                                Some(SendUserMessageOptions {
                                    deliver_as: Some(SendUserMessageDeliverAs::FollowUp),
                                    expand_prompt_templates: None,
                                }),
                            );
                            Ok(None)
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        let sink = Arc::clone(&lifecycle);
        let _unsubscribe = harness
            .session
            .subscribe(Arc::new(move |event| match event {
                AgentSessionEvent::AgentEnd { .. } => {
                    sink.lock().unwrap().push("agent_end".to_string())
                }
                AgentSessionEvent::AutoRetryStart { .. } => {
                    sink.lock().unwrap().push("auto_retry_start".to_string())
                }
                _ => {}
            }));
        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                "",
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::Error),
                    error_message: Some("overloaded_error".to_string()),
                    ..Default::default()
                },
            )),
            capturing_response(Arc::clone(&requests), "retry recovered"),
            capturing_response(Arc::clone(&requests), "follow-up completed"),
        ]);

        harness
            .session
            .prompt("start".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(harness.faux.state().call_count(), 3);
        let requests = requests.lock().unwrap();
        assert!(!requests[0].contains("queued follow-up"));
        assert!(requests[1].contains("queued follow-up"));
        let lifecycle = lifecycle.lock().unwrap();
        assert_eq!(
            lifecycle.iter().take(2).cloned().collect::<Vec<_>>(),
            vec!["agent_end".to_string(), "auto_retry_start".to_string()]
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "marks the exhausted retry run as final"
#[tokio::test]
async fn marks_the_exhausted_retry_run_as_final() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_from_json(json!({
                "retry": { "enabled": true, "maxRetries": 1, "baseDelayMs": 1 }
            }))),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                "",
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::Error),
                    error_message: Some("overloaded_error".to_string()),
                    ..Default::default()
                },
            )),
            FauxResponseStep::Message(faux_assistant_message(
                "",
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::Error),
                    error_message: Some("overloaded_error".to_string()),
                    ..Default::default()
                },
            )),
        ]);

        harness
            .session
            .prompt("start".to_string(), PromptOptions::default())
            .await
            .unwrap();

        let will_retry: Vec<bool> = harness
            .events_of_type("agent_end")
            .into_iter()
            .filter_map(|event| match event {
                AgentSessionEvent::AgentEnd { will_retry, .. } => Some(will_retry),
                _ => None,
            })
            .collect();
        assert_eq!(will_retry, vec![true, false]);
        let end = harness
            .events_of_type("auto_retry_end")
            .into_iter()
            .next_back()
            .expect("auto_retry_end");
        match end {
            AgentSessionEvent::AutoRetryEnd {
                success, attempt, ..
            } => {
                assert!(!success);
                assert_eq!(attempt, 1);
            }
            other => panic!("unexpected event: {other:?}"),
        }
        harness.cleanup();
    })
    .await;
}

/// TS: "keeps omissions and does not retry when recovery compaction fails"
#[tokio::test]
async fn keeps_omissions_and_does_not_retry_when_recovery_compaction_fails() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(1000),
                max_tokens: Some(100),
                ..Default::default()
            }]),
            settings: Some(settings_from_json(json!({
                "compaction": { "keepRecentTokens": 1, "reserveTokens": 0 },
                "retry": { "enabled": false, "maxRetries": 0, "baseDelayMs": 1 }
            }))),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                "partial response",
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::Length),
                    ..Default::default()
                },
            )),
            FauxResponseStep::Message(faux_assistant_message(
                "summary failed",
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::Error),
                    error_message: Some("summary failed".to_string()),
                    ..Default::default()
                },
            )),
            faux_text_response("must not retry"),
        ]);

        harness
            .session
            .prompt("x".repeat(5000), PromptOptions::default())
            .await
            .unwrap();

        let entries = harness.session_manager().get_entries();
        assert!(
            entries
                .iter()
                .any(|entry| matches!(entry, SessionEntry::ContextEdit(_)))
        );
        assert!(
            !entries
                .iter()
                .any(|entry| matches!(entry, SessionEntry::Compaction(_)))
        );
        assert!(entries.iter().any(|entry| match entry {
            SessionEntry::Message(message) => {
                get_message_text(&message.message) == "partial response"
            }
            _ => false,
        }));
        assert!(
            !harness
                .session_manager()
                .build_session_projection()
                .messages
                .iter()
                .any(|message| get_message_text(message) == "partial response")
        );
        assert_eq!(harness.faux.state().call_count(), 2);
        harness.cleanup();
    })
    .await;
}

// 保持未使用导入不告警。
#[allow(dead_code)]
fn _unused(_: CompactionEntryDraft, _: SessionMessageType, _: &AgentTool) {
    let _ = ExtensionEventKind::TurnStart;
}
