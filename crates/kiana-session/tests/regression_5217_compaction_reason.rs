//! `test/suite/regressions/5217-compaction-reason.test.ts` 的 Rust 移植。

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use kiana_compaction::compaction::CompactionResult;
use kiana_ext_api::events::{
    CompactionReason, ExtensionEvent, ExtensionEventKind, ExtensionEventResult,
    SessionBeforeCompactResult, SessionEvent,
};
use kiana_session::agent_session::PromptOptions;

#[derive(Clone, Debug, PartialEq, Eq)]
struct RecordedCompactionEvent {
    event_type: &'static str,
    reason: CompactionReason,
    will_retry: bool,
}

fn recording_extension(
    recorded: Arc<Mutex<Vec<RecordedCompactionEvent>>>,
) -> kiana_ext_api::types::ExtensionFactory {
    factory(move |kiana| {
        let before = Arc::clone(&recorded);
        let _ = kiana.on(
            ExtensionEventKind::SessionBeforeCompact,
            standard(move |event, _ctx| {
                let before = Arc::clone(&before);
                Box::pin(async move {
                    let ExtensionEvent::Session(SessionEvent::BeforeCompact(event)) = event else {
                        return Ok(None);
                    };
                    before.lock().unwrap().push(RecordedCompactionEvent {
                        event_type: "session_before_compact",
                        reason: event.reason,
                        will_retry: event.will_retry,
                    });
                    Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                        SessionBeforeCompactResult {
                            cancel: None,
                            compaction: Some(CompactionResult {
                                summary: "summary from extension".to_string(),
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
        let after = Arc::clone(&recorded);
        let _ = kiana.on(
            ExtensionEventKind::SessionCompact,
            standard(move |event, _ctx| {
                let after = Arc::clone(&after);
                Box::pin(async move {
                    let ExtensionEvent::Session(SessionEvent::Compact(event)) = event else {
                        return Ok(None);
                    };
                    after.lock().unwrap().push(RecordedCompactionEvent {
                        event_type: "session_compact",
                        reason: event.reason,
                        will_retry: event.will_retry,
                    });
                    Ok(None)
                })
            }),
        );
    })
}

async fn create_compaction_harness(recorded: Arc<Mutex<Vec<RecordedCompactionEvent>>>) -> Harness {
    let harness = create_harness(HarnessOptions {
        settings: Some(settings_from_json(serde_json::json!({
            "compaction": { "keepRecentTokens": 1 }
        }))),
        extension_factories: vec![recording_extension(recorded)],
        ..Default::default()
    })
    .await;
    harness.set_responses(vec![faux_text_response("one"), faux_text_response("two")]);
    harness
        .session
        .prompt("first".to_string(), PromptOptions::default())
        .await
        .unwrap();
    harness
        .session
        .prompt("second".to_string(), PromptOptions::default())
        .await
        .unwrap();
    harness
}

/// TS: "reports manual reason for compact()"
#[tokio::test]
async fn reports_manual_reason_for_compact() {
    run(async {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let harness = create_compaction_harness(Arc::clone(&recorded)).await;
        harness.session.compact(None).await.unwrap();
        assert_eq!(
            *recorded.lock().unwrap(),
            vec![
                RecordedCompactionEvent {
                    event_type: "session_before_compact",
                    reason: CompactionReason::Manual,
                    will_retry: false,
                },
                RecordedCompactionEvent {
                    event_type: "session_compact",
                    reason: CompactionReason::Manual,
                    will_retry: false,
                },
            ]
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "reports threshold reason for auto-compaction"
#[tokio::test]
async fn reports_threshold_reason_for_auto_compaction() {
    run(async {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let harness = create_compaction_harness(Arc::clone(&recorded)).await;
        harness
            .session
            .run_auto_compaction(CompactionReason::Threshold, false)
            .await;
        assert_eq!(
            *recorded.lock().unwrap(),
            vec![
                RecordedCompactionEvent {
                    event_type: "session_before_compact",
                    reason: CompactionReason::Threshold,
                    will_retry: false,
                },
                RecordedCompactionEvent {
                    event_type: "session_compact",
                    reason: CompactionReason::Threshold,
                    will_retry: false,
                },
            ]
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "reports overflow reason and willRetry for overflow recovery"
#[tokio::test]
async fn reports_overflow_reason_and_will_retry_for_overflow_recovery() {
    run(async {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let harness = create_compaction_harness(Arc::clone(&recorded)).await;
        harness
            .session
            .run_auto_compaction(CompactionReason::Overflow, true)
            .await;
        assert_eq!(
            *recorded.lock().unwrap(),
            vec![
                RecordedCompactionEvent {
                    event_type: "session_before_compact",
                    reason: CompactionReason::Overflow,
                    will_retry: true,
                },
                RecordedCompactionEvent {
                    event_type: "session_compact",
                    reason: CompactionReason::Overflow,
                    will_retry: true,
                },
            ]
        );
        harness.cleanup();
    })
    .await;
}
