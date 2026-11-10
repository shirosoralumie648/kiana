//! 回归（fixer, minor）：`navigateTree` 的扩展摘要为空字符串时按 TS 的 truthiness 处理。
//!
//! TS: agent-session.ts:4175-4240 —— `if (summaryText)` 为假：不创建 `branch_summary`
//! 条目、标签留在目标条目上、`session_tree.summaryEntry`/`fromExtension` 为 undefined。
#![allow(clippy::arc_with_non_send_sync)]

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use kiana_ext_api::events::{
    ExtensionEvent, ExtensionEventKind, ExtensionEventResult, SessionBeforeTreeResult,
    SessionEvent, SessionTreeEvent, TreeSummary,
};
use kiana_ext_api::types::ExtensionApi;
use kiana_session::agent_session::NavigateTreeOptions;
use kiana_session_core::session_manager::SessionEntry;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn assistant_entries(harness: &Harness) -> Vec<SessionEntry> {
    harness
        .session_entries()
        .into_iter()
        .filter(|entry| match entry {
            SessionEntry::Message(message) => message.message.role() == "assistant",
            _ => false,
        })
        .collect()
}

fn user_entries(harness: &Harness) -> Vec<SessionEntry> {
    harness
        .session_entries()
        .into_iter()
        .filter(|entry| match entry {
            SessionEntry::Message(message) => message.message.role() == "user",
            _ => false,
        })
        .collect()
}

#[tokio::test]
async fn empty_extension_summary_is_treated_as_no_summary() {
    run(async {
        let seen_tree: Arc<Mutex<Vec<SessionTreeEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen_tree);
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(move |kiana: Arc<dyn ExtensionApi>| {
                let _ = kiana.on(
                    ExtensionEventKind::SessionBeforeTree,
                    standard(|_event, _ctx| {
                        Box::pin(async move {
                            Ok(Some(ExtensionEventResult::SessionBeforeTree(
                                SessionBeforeTreeResult {
                                    cancel: None,
                                    summary: Some(TreeSummary {
                                        summary: String::new(),
                                        details: None,
                                        usage: None,
                                    }),
                                    custom_instructions: None,
                                    replace_instructions: None,
                                    label: None,
                                },
                            )))
                        })
                    }),
                );
                let sink = Arc::clone(&sink);
                let _ = kiana.on(
                    ExtensionEventKind::SessionTree,
                    standard(move |event, _ctx| {
                        let sink = Arc::clone(&sink);
                        Box::pin(async move {
                            let ExtensionEvent::Session(SessionEvent::Tree(tree)) = event else {
                                return Ok(None);
                            };
                            lock(&sink).push(tree);
                            Ok(None)
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            faux_text_response("answer 1"),
            faux_text_response("answer 2"),
        ]);
        harness
            .session
            .prompt("First".to_string(), Default::default())
            .await
            .unwrap();
        harness
            .session
            .prompt("Second".to_string(), Default::default())
            .await
            .unwrap();

        let target = user_entries(&harness)
            .into_iter()
            .next()
            .expect("first user message");
        let target_id = target.id().to_string();
        assert!(!assistant_entries(&harness).is_empty());

        let result = harness
            .session
            .navigate_tree(
                target_id.clone(),
                NavigateTreeOptions {
                    summarize: Some(true),
                    label: Some("keep".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // 空摘要 = 没有摘要：不创建 branch_summary 条目。
        assert!(result.summary_entry.is_none());
        assert!(
            harness
                .session_entries()
                .into_iter()
                .all(|entry| entry.kind() != "branch_summary"),
            "no branch summary entry"
        );
        // 标签留在目标条目上（不是摘要条目）。
        assert_eq!(
            harness.session_manager().get_label(&target_id),
            Some("keep")
        );
        // session_tree 事件里 summaryEntry/fromExtension 都是 undefined。
        let events = lock(&seen_tree);
        let tree_event = events.last().expect("session_tree event");
        assert!(tree_event.summary_entry.is_none());
        assert!(tree_event.from_extension.is_none());
        harness.cleanup();
    })
    .await;
}
