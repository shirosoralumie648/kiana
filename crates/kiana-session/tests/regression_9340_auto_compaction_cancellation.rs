//! `test/suite/regressions/9340-9777-auto-compaction-cancellation.test.ts` 的 Rust 移植。
//!
//! 未移植：`cancels summarization authentication`（需要 mock `ModelRuntime::getAuth`，
//! 在 Rust 中没有等价的注入点）；其余 5 个用例按可观察行为移植。

mod common;

use std::sync::Arc;

use common::*;
use kiana_agent_core::types::AgentMessage;
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message,
};
use kiana_ai::types::{Message, StopReason, Usage, UsageCost, UserContent, UserMessage, UserRole};
use kiana_ext_api::events::{
    CompactionReason, ExtensionEventKind, ExtensionEventResult, SessionBeforeCompactResult,
};
use kiana_session::agent_session::{AgentSessionEvent, PromptOptions};
use serde_json::json;

fn seed_compactable_session(harness: &Harness) {
    let model = harness.get_model(None).expect("model");
    harness.append_session_message(AgentMessage::Standard(Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text("x".repeat(500)),
        timestamp: 1,
    })));
    let mut assistant = faux_assistant_message("y".repeat(200), Default::default());
    assistant.api = model.api.clone();
    assistant.provider = model.provider.clone();
    assistant.model = model.id.clone();
    assistant.timestamp = 2;
    assistant.usage = Usage {
        input: 100,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: None,
        reasoning: None,
        total_tokens: 100,
        cost: UsageCost::default(),
    };
    harness.append_session_message(AgentMessage::Standard(Message::Assistant(assistant)));
}

/// TS: "does not start post-run auto-compaction after abort"（#9340）
#[tokio::test]
async fn does_not_start_post_run_auto_compaction_after_abort() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(200),
                max_tokens: Some(50),
                ..Default::default()
            }]),
            settings: Some(settings_from_json(json!({
                "compaction": { "enabled": true, "reserveTokens": 50, "keepRecentTokens": 1 },
                "retry": { "enabled": false }
            }))),
            extension_factories: vec![factory(|kiana| {
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
            })],
            ..Default::default()
        })
        .await;
        seed_compactable_session(&harness);
        harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            "",
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some("Synthetic network failure".to_string()),
                ..Default::default()
            },
        ))]);
        let session = harness.session.clone();
        let _unsubscribe = harness.session.subscribe(Arc::new(move |event| {
            if let AgentSessionEvent::MessageEnd { message } = event
                && message.role() == "assistant"
            {
                session.abort_compaction();
                // 先同步置位 abort 标志，再异步执行 abort()，确保标志在事件回调返回前生效。
                session.request_abort();
                let session = session.clone();
                tokio::task::spawn_local(async move { session.abort().await });
            }
        }));

        let _ = harness
            .session
            .prompt("z".repeat(1000), PromptOptions::default())
            .await;

        assert!(harness.events_of_type("compaction_start").is_empty());
        harness.cleanup();
    })
    .await;
}

/// TS: "cancels synchronously from compaction_start"（#9777）
#[tokio::test]
async fn cancels_synchronously_from_compaction_start() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_from_json(json!({
                "compaction": { "keepRecentTokens": 1 }
            }))),
            ..Default::default()
        })
        .await;
        seed_compactable_session(&harness);
        let session = harness.session.clone();
        let _unsubscribe = harness.session.subscribe(Arc::new(move |event| {
            if let AgentSessionEvent::CompactionStart { .. } = event {
                session.abort_compaction();
            }
        }));

        let resumed = harness
            .session
            .run_auto_compaction(CompactionReason::Threshold, false)
            .await;

        assert!(!resumed);
        assert_eq!(harness.faux.state().call_count(), 0);
        match harness
            .events_of_type("compaction_end")
            .into_iter()
            .next_back()
            .expect("compaction_end")
        {
            AgentSessionEvent::CompactionEnd { aborted, .. } => assert!(aborted),
            other => panic!("unexpected event: {other:?}"),
        }
        harness.cleanup();
    })
    .await;
}

async fn run_failure_with_message(message: &str) {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_from_json(json!({
                "compaction": { "keepRecentTokens": 1 }
            }))),
            ..Default::default()
        })
        .await;
        seed_compactable_session(&harness);
        let error_message = message.to_string();
        harness
            .agent
            .set_stream_function(Arc::new(move |_model, _context, _options| {
                let error_message = error_message.clone();
                Box::pin(async move {
                    Err(kiana_ai::AiError::new(
                        kiana_ai::AiErrorKind::Other,
                        error_message,
                    ))
                })
            }));

        harness
            .session
            .run_auto_compaction(CompactionReason::Threshold, false)
            .await;

        let end = harness
            .events_of_type("compaction_end")
            .into_iter()
            .next_back()
            .expect("compaction_end");
        match end {
            AgentSessionEvent::CompactionEnd {
                aborted,
                error_message,
                ..
            } => {
                assert!(!aborted);
                assert!(
                    error_message
                        .as_deref()
                        .is_some_and(|text| text.contains(message)),
                    "unexpected error: {error_message:?}"
                );
            }
            other => panic!("unexpected event: {other:?}"),
        }
        harness.cleanup();
    })
    .await;
}

/// TS: `it.each` "reports matching error text as a failure"
#[tokio::test]
async fn reports_matching_compaction_cancelled_error_text_as_failure() {
    run_failure_with_message("Compaction cancelled").await;
}

/// TS: `it.each` "reports an unrelated AbortError as a failure"
#[tokio::test]
async fn reports_unrelated_auth_error_as_failure() {
    run_failure_with_message("auth failed").await;
}

/// TS: "reports extension cancellation as aborted"
#[tokio::test]
async fn reports_extension_cancellation_as_aborted() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_from_json(json!({
                "compaction": { "keepRecentTokens": 1 }
            }))),
            extension_factories: vec![factory(|kiana| {
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
            })],
            ..Default::default()
        })
        .await;
        seed_compactable_session(&harness);

        harness
            .session
            .run_auto_compaction(CompactionReason::Threshold, false)
            .await;

        match harness
            .events_of_type("compaction_end")
            .into_iter()
            .next_back()
            .expect("compaction_end")
        {
            AgentSessionEvent::CompactionEnd { aborted, .. } => assert!(aborted),
            other => panic!("unexpected event: {other:?}"),
        }
        harness.cleanup();
    })
    .await;
}

// 未移植：`cancels summarization authentication`（需要 mock `ModelRuntime::getAuth`）。
