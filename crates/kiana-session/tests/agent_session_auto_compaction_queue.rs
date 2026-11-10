//! 自动压缩与 idle 等待的回归测试（覆盖 TS auto-compaction 队列/取消语义的核心）。
//!
//! TS 源：`test/agent-session-auto-compaction-queue.test.ts` 与
//! `test/suite/regressions/9340-9777-auto-compaction-cancellation.test.ts` 的关键路径。

mod common;

use common::*;
use kiana_ext_api::events::{
    ExtensionEvent, ExtensionEventKind, ExtensionEventResult, SessionBeforeCompactResult,
};
use kiana_session_core::session_manager::SessionEntry;
use serde_json::json;

/// 自动阈值压缩结束后：`isCompacting` 必须为 false、`isIdle` 为 true，且 `waitForIdle`
/// 不会挂起（对应 TS `_runAutoCompaction` 的 `finally`）。
#[tokio::test]
async fn auto_compaction_clears_controller_and_resolves_idle_waiters() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![kiana_ai::providers::faux::FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(200),
                max_tokens: Some(50),
                ..Default::default()
            }]),
            settings: Some(settings_from_json(json!({
                "compaction": {"enabled": true, "keepRecentTokens": 1, "reserveTokens": 0},
                "retry": {"enabled": false}
            }))),
            extension_factories: vec![factory(|kiana| {
                let _ = kiana.on(
                    ExtensionEventKind::SessionBeforeCompact,
                    standard(|event, _ctx| {
                        Box::pin(async move {
                            let ExtensionEvent::Session(
                                kiana_ext_api::events::SessionEvent::BeforeCompact(before),
                            ) = event
                            else {
                                return Ok(None);
                            };
                            let mut compaction = kiana_compaction::compaction::CompactionResult {
                                summary: "auto summary".to_string(),
                                first_kept_entry_id: before.preparation.first_kept_entry_id.clone(),
                                tokens_before: before.preparation.tokens_before,
                                estimated_tokens_after: None,
                                usage: None,
                                details: None,
                            };
                            let _ = &mut compaction;
                            Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                                SessionBeforeCompactResult {
                                    cancel: None,
                                    compaction: Some(compaction),
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
            faux_text_response("response"),
            faux_text_response("continued response"),
        ]);
        harness
            .session
            .prompt("x".repeat(1000), Default::default())
            .await
            .unwrap();
        assert!(!harness.events_of_type("compaction_start").is_empty());
        assert!(!harness.session.is_compacting());
        assert!(harness.session.is_idle());
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            harness.session.wait_for_idle(),
        )
        .await
        .expect("waitForIdle must resolve");
        let compaction = harness
            .session_entries()
            .into_iter()
            .find(|entry| entry.kind() == "compaction")
            .expect("compaction entry");
        if let SessionEntry::Compaction(entry) = compaction {
            assert_eq!(entry.summary, "auto summary");
        }
        // 后续 manual compact 不应因残留 controller 而在初始 abort() 挂起。
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            harness
                .session
                .prompt("again".to_string(), Default::default()),
        )
        .await
        .expect("prompt after auto-compaction must not hang")
        .unwrap();
        harness.cleanup();
    })
    .await;
}

/// 多个 `waitForIdle` 等待者必须都在会话真正空闲后返回（共享 promise 语义）。
#[tokio::test]
async fn wait_for_idle_waits_for_every_waiter() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![faux_text_response("ok")]);
        let first = harness.session.clone();
        let second = harness.session.clone();
        let waiters = tokio::task::spawn_local(async move {
            let a = tokio::task::spawn_local(async move { first.wait_for_idle().await });
            let b = tokio::task::spawn_local(async move { second.wait_for_idle().await });
            (a.await.unwrap(), b.await.unwrap())
        });
        // 让两个等待者登记。
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        harness
            .session
            .prompt("hi".to_string(), Default::default())
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), waiters)
            .await
            .expect("all waiters must resolve")
            .unwrap();
        harness.cleanup();
    })
    .await;
}
