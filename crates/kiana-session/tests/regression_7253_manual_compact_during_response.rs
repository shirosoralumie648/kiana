//! `test/suite/regressions/7253-manual-compact-during-response.test.ts` 的 Rust 移植。

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxModelDefinition, FauxResponseStep, faux_assistant_message,
    faux_tool_call,
};
use kiana_ai::types::StopReason;
use kiana_compaction::compaction::CompactionResult;
use kiana_ext_api::events::{
    CompactionReason, ExtensionEvent, ExtensionEventKind, ExtensionEventResult,
    SessionBeforeCompactResult, SessionEvent,
};
use kiana_session::agent_session::PromptOptions;
use kiana_session_core::session_manager::SessionEntry;
use tokio::sync::Notify;

fn compact_summary_extension() -> kiana_ext_api::types::ExtensionFactory {
    factory(|kiana| {
        let _ = kiana.on(
            ExtensionEventKind::SessionBeforeCompact,
            standard(move |event, _ctx| {
                Box::pin(async move {
                    let ExtensionEvent::Session(SessionEvent::BeforeCompact(event)) = event else {
                        return Ok(None);
                    };
                    let reason = match event.reason {
                        CompactionReason::Manual => "manual",
                        CompactionReason::Threshold => "threshold",
                        CompactionReason::Overflow => "overflow",
                    };
                    Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                        SessionBeforeCompactResult {
                            cancel: None,
                            compaction: Some(CompactionResult {
                                summary: format!("{reason} summary"),
                                first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                                tokens_before: event.preparation.tokens_before,
                                estimated_tokens_after: None,
                                usage: None,
                                details: Some(serde_json::json!({})),
                            }),
                        },
                    )))
                })
            }),
        );
    })
}

/// TS: "persists the aborted response before running the requested manual compaction"
#[tokio::test]
async fn persists_the_aborted_response_before_running_the_requested_manual_compaction() {
    run(async {
        let (noop_tool, tool_notify) = WaitTool::make("done");
        let noop_tool = {
            let mut tool = noop_tool;
            tool.name = "noop".to_string();
            tool.label = "No-op".to_string();
            tool
        };
        let harness = create_harness(HarnessOptions {
            models: Some(vec![FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(1000),
                max_tokens: Some(1000),
                ..Default::default()
            }]),
            settings: Some(settings_from_json(serde_json::json!({
                "compaction": { "enabled": true, "reserveTokens": 200, "keepRecentTokens": 2 }
            }))),
            tools: Some(vec![noop_tool]),
            extension_factories: vec![compact_summary_extension()],
            ..Default::default()
        })
        .await;

        let second_started = Arc::new(Notify::new());
        let second_released = Arc::new(Notify::new());
        let started = Arc::clone(&second_started);
        let released = Arc::clone(&second_released);
        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                faux_tool_call("noop", kiana_ai::types::JsonObject::new()),
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::ToolUse),
                    ..Default::default()
                },
            )),
            FauxResponseStep::message_factory(move |_input| {
                let started = Arc::clone(&started);
                let released = Arc::clone(&released);
                async move {
                    started.notify_one();
                    released.notified().await;
                    faux_assistant_message(
                        format!("second response:{}", "x".repeat(4000)),
                        Default::default(),
                    )
                }
            }),
        ]);

        let prompt_fut = harness.session.prompt(
            "Run the tool, then continue responding.".to_string(),
            PromptOptions::default(),
        );
        let compact_fut = async {
            second_started.notified().await;
            let compact = harness.session.compact(None);
            let release = async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                second_released.notify_one();
            };
            let (result, ()) = futures::join!(compact, release);
            result
        };
        let release_tool = async {
            // 工具立即完成（TS 的 noop）。
            tool_notify.notify_one();
        };
        let (prompt_result, compact_result, ()) =
            futures::join!(prompt_fut, compact_fut, release_tool);
        prompt_result.unwrap();
        let compact_result = compact_result.unwrap();
        assert_eq!(compact_result.summary, "manual summary");

        let starts: Vec<CompactionReason> = harness
            .events_of_type("compaction_start")
            .into_iter()
            .filter_map(|event| match event {
                kiana_session::agent_session::AgentSessionEvent::CompactionStart { reason } => {
                    Some(reason)
                }
                _ => None,
            })
            .collect();
        assert_eq!(starts, vec![CompactionReason::Manual]);
        let ends: Vec<CompactionReason> = harness
            .events_of_type("compaction_end")
            .into_iter()
            .filter_map(|event| match event {
                kiana_session::agent_session::AgentSessionEvent::CompactionEnd {
                    reason, ..
                } => Some(reason),
                _ => None,
            })
            .collect();
        assert_eq!(ends, vec![CompactionReason::Manual]);

        let entries = harness.session_entries();
        let aborted_index = entries.iter().position(|entry| match entry {
            SessionEntry::Message(message) => match &message.message {
                kiana_agent_core::types::AgentMessage::Standard(
                    kiana_ai::types::Message::Assistant(assistant),
                ) => assistant.stop_reason == StopReason::Aborted,
                _ => false,
            },
            _ => false,
        });
        let compaction_index = entries
            .iter()
            .position(|entry| matches!(entry, SessionEntry::Compaction(_)));
        let aborted_index = aborted_index.expect("aborted assistant persisted");
        let compaction_index = compaction_index.expect("compaction persisted");
        assert!(compaction_index > aborted_index);
        assert_eq!(
            entries
                .iter()
                .filter(|entry| matches!(entry, SessionEntry::Compaction(_)))
                .count(),
            1
        );
        harness.cleanup();
    })
    .await;
}
