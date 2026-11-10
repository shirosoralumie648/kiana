//! `regressions/3688-tree-cancel-compacting.test.ts` 的 Rust 移植（issue #3688）。

mod common;

use common::*;
use kiana_ext_api::events::{ExtensionEventKind, ExtensionEventResult, SessionBeforeTreeResult};
use kiana_session_core::session_manager::SessionManager;

/// TS: "clears branch summary state when session_before_tree cancels navigation"
#[tokio::test]
async fn clears_branch_summary_state_when_session_before_tree_cancels_navigation() {
    run(async {
        let mut manager = SessionManager::in_memory("/tmp", None, None).expect("manager");
        let target_id = manager.append_message(user_msg("first")).unwrap();
        manager.append_message(assistant_msg("reply")).unwrap();
        let current_leaf = manager.append_message(user_msg("second")).unwrap();
        let harness = create_harness(HarnessOptions {
            session_manager: Some(manager),
            extension_factories: vec![factory(|kiana| {
                let _ = kiana.on(
                    ExtensionEventKind::SessionBeforeTree,
                    standard(move |_event, _ctx| {
                        Box::pin(async move {
                            Ok(Some(ExtensionEventResult::SessionBeforeTree(
                                SessionBeforeTreeResult {
                                    cancel: Some(true),
                                    ..Default::default()
                                },
                            )))
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        assert_eq!(
            harness.session.session_manager().get_leaf_id(),
            Some(current_leaf.as_str())
        );
        let result = harness
            .session
            .navigate_tree(target_id, Default::default())
            .await
            .unwrap();
        assert!(result.cancelled);
        assert!(!harness.session.is_compacting());
        assert_eq!(
            harness.session.session_manager().get_leaf_id(),
            Some(current_leaf.as_str())
        );
        harness.cleanup();
    })
    .await;
}
