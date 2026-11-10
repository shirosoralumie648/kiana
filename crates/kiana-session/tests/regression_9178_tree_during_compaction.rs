//! `test/suite/regressions/9178-tree-during-compaction.test.ts` 的 Rust 移植。

mod common;

use std::sync::Arc;

use common::*;
use kiana_compaction::compaction::CompactionResult;
use kiana_ext_api::events::{
    ExtensionEvent, ExtensionEventKind, ExtensionEventResult, SessionBeforeCompactResult,
    SessionEvent,
};
use kiana_session::agent_session::NavigateTreeOptions;
use kiana_session_core::session_manager::SessionEntry;
use tokio::sync::Notify;

fn compact_summary_extension(
    started: Arc<Notify>,
    released: Arc<Notify>,
) -> kiana_ext_api::types::ExtensionFactory {
    factory(move |kiana| {
        let started = Arc::clone(&started);
        let released = Arc::clone(&released);
        let _ = kiana.on(
            ExtensionEventKind::SessionBeforeCompact,
            standard(move |event, _ctx| {
                let started = Arc::clone(&started);
                let released = Arc::clone(&released);
                Box::pin(async move {
                    let ExtensionEvent::Session(SessionEvent::BeforeCompact(event)) = event else {
                        return Ok(None);
                    };
                    started.notify_one();
                    released.notified().await;
                    Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                        SessionBeforeCompactResult {
                            cancel: None,
                            compaction: Some(CompactionResult {
                                summary: "summary".to_string(),
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

/// TS: "rejects navigation before the active leaf can change"
#[tokio::test]
async fn rejects_navigation_before_the_active_leaf_can_change() {
    run(async {
        let started = Arc::new(Notify::new());
        let released = Arc::new(Notify::new());
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_from_json(serde_json::json!({
                "compaction": { "keepRecentTokens": 1 }
            }))),
            extension_factories: vec![compact_summary_extension(
                Arc::clone(&started),
                Arc::clone(&released),
            )],
            ..Default::default()
        })
        .await;
        let _first_user = harness.append_session_message(user_msg("first user"));
        let navigation_target = harness.append_session_message(assistant_msg("first assistant"));
        let _second_user = harness.append_session_message(user_msg("second user"));
        let original_leaf = harness.append_session_message(assistant_msg("second assistant"));

        let compaction_fut = harness.session.compact(None);
        let driver = async {
            started.notified().await;
            assert!(harness.session.is_compacting());
            let error = harness
                .session
                .navigate_tree(
                    navigation_target.clone(),
                    NavigateTreeOptions {
                        summarize: Some(false),
                        ..Default::default()
                    },
                )
                .await
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                "Wait for the current compaction or tree navigation to finish before navigating the session tree."
            );
            assert_eq!(
                harness
                    .session_manager()
                    .get_leaf_id()
                    .map(str::to_string),
                Some(original_leaf.clone())
            );
            released.notify_one();
        };
        let (compaction_result, ()) = futures::join!(compaction_fut, driver);
        compaction_result.unwrap();

        let entries = harness.session_entries();
        let last = entries.last().expect("entry");
        let SessionEntry::Compaction(entry) = last else {
            panic!("expected compaction entry");
        };
        assert_eq!(entry.parent_id.as_deref(), Some(original_leaf.as_str()));
        assert!(harness
            .session
            .messages()
            .iter()
            .any(|message| get_message_text(message) == "second assistant"));
        harness.cleanup();
    })
    .await;
}

/// TS: "rejects a second navigation while the first is waiting"
#[tokio::test]
async fn rejects_a_second_navigation_while_the_first_is_waiting() {
    run(async {
        let started = Arc::new(Notify::new());
        let released = Arc::new(Notify::new());
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory({
                let started = Arc::clone(&started);
                let released = Arc::clone(&released);
                move |kiana| {
                    let started = Arc::clone(&started);
                    let released = Arc::clone(&released);
                    let _ = kiana.on(
                        ExtensionEventKind::SessionBeforeTree,
                        standard(move |_event, _ctx| {
                            let started = Arc::clone(&started);
                            let released = Arc::clone(&released);
                            Box::pin(async move {
                                started.notify_one();
                                released.notified().await;
                                Ok(None)
                            })
                        }),
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        let second_target = harness.append_session_message(user_msg("first user"));
        let first_target = harness.append_session_message(assistant_msg("first assistant"));
        let _second_user = harness.append_session_message(user_msg("second user"));
        let original_leaf = harness.append_session_message(assistant_msg("second assistant"));

        let first_navigation = harness.session.navigate_tree(
            first_target.clone(),
            NavigateTreeOptions {
                summarize: Some(false),
                ..Default::default()
            },
        );
        let driver = async {
            started.notified().await;
            let second = harness
                .session
                .navigate_tree(
                    second_target,
                    NavigateTreeOptions {
                        summarize: Some(false),
                        ..Default::default()
                    },
                )
                .await;
            assert_eq!(
                second.unwrap_err().to_string(),
                "Wait for the current compaction or tree navigation to finish before navigating the session tree."
            );
            assert_eq!(
                harness
                    .session_manager()
                    .get_leaf_id()
                    .map(str::to_string),
                Some(original_leaf)
            );
            released.notify_one();
        };
        let (first_result, ()) = futures::join!(first_navigation, driver);
        first_result.unwrap();
        assert_eq!(
            harness
                .session_manager()
                .get_leaf_id()
                .map(str::to_string),
            Some(first_target)
        );
        harness.cleanup();
    })
    .await;
}
