//! 回归（fixer, minor）：`session_compact.compactionEntry` 用会话里**第一条**摘要相同的
//! 压缩条目（TS `newEntries.find(...)`），而不是刚追加的那条。
#![allow(clippy::arc_with_non_send_sync)]

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use kiana_compaction::compaction::CompactionResult;
use kiana_ext_api::events::{
    ExtensionEvent, ExtensionEventKind, ExtensionEventResult, SessionBeforeCompactResult,
    SessionCompactEvent, SessionEvent,
};
use kiana_ext_api::types::ExtensionApi;
use kiana_session_core::session_manager::SessionEntry;
use serde_json::json;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[tokio::test]
async fn session_compact_uses_the_first_entry_with_the_same_summary() {
    run(async {
        let events: Arc<Mutex<Vec<SessionCompactEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_from_json(json!({
                "compaction": {"keepRecentTokens": 1}
            }))),
            extension_factories: vec![factory(move |kiana: Arc<dyn ExtensionApi>| {
                let _ = kiana.on(
                    ExtensionEventKind::SessionBeforeCompact,
                    standard(|event, _ctx| {
                        Box::pin(async move {
                            let ExtensionEvent::Session(SessionEvent::BeforeCompact(before)) =
                                event
                            else {
                                return Ok(None);
                            };
                            Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                                SessionBeforeCompactResult {
                                    cancel: None,
                                    compaction: Some(CompactionResult {
                                        summary: "same summary".to_string(),
                                        first_kept_entry_id: before
                                            .preparation
                                            .first_kept_entry_id
                                            .clone(),
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
                let sink = Arc::clone(&sink);
                let _ = kiana.on(
                    ExtensionEventKind::SessionCompact,
                    standard(move |event, _ctx| {
                        let sink = Arc::clone(&sink);
                        Box::pin(async move {
                            let ExtensionEvent::Session(SessionEvent::Compact(compact)) = event
                            else {
                                return Ok(None);
                            };
                            lock(&sink).push(compact);
                            Ok(None)
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            faux_text_response("answer one"),
            faux_text_response("answer two"),
            faux_text_response("answer three"),
            faux_text_response("answer four"),
        ]);
        harness
            .session
            .prompt("one".to_string(), Default::default())
            .await
            .unwrap();
        harness
            .session
            .prompt("two".to_string(), Default::default())
            .await
            .unwrap();
        let first = harness.session.compact(None).await.expect("first compact");
        assert_eq!(first.summary, "same summary");
        harness
            .session
            .prompt("three".to_string(), Default::default())
            .await
            .unwrap();
        harness
            .session
            .prompt("four".to_string(), Default::default())
            .await
            .unwrap();
        let second = harness.session.compact(None).await.expect("second compact");
        assert_eq!(second.summary, "same summary");

        let compact_entries: Vec<SessionEntry> = harness
            .session_entries()
            .into_iter()
            .filter(|entry| entry.kind() == "compaction")
            .collect();
        assert_eq!(compact_entries.len(), 2, "two compaction entries persisted");

        let events = lock(&events);
        assert_eq!(events.len(), 2, "two session_compact events");
        assert_eq!(
            events[0].compaction_entry.id, events[1].compaction_entry.id,
            "both events carry the first matching entry"
        );
        let latest = compact_entries
            .last()
            .expect("latest compaction")
            .id()
            .to_string();
        let first_event = &events[0].compaction_entry.id;
        assert_ne!(
            first_event, &latest,
            "the second event must not use the newly appended entry"
        );
        harness.cleanup();
    })
    .await;
}
