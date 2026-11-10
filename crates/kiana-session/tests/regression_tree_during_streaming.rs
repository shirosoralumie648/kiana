//! `regressions/tree-during-streaming.test.ts` 的 Rust 移植。

mod common;

use common::*;
use kiana_ai::providers::faux::FauxResponseStep;
use kiana_session_core::session_manager::SessionManager;

/// TS: "rejects navigation without changing the active leaf"
#[tokio::test]
async fn rejects_navigation_without_changing_the_active_leaf() {
    run(async {
        let mut manager = SessionManager::in_memory("/tmp", None, None).expect("manager");
        let target_id = manager.append_message(user_msg("first")).unwrap();
        let harness = create_harness(HarnessOptions {
            session_manager: Some(manager),
            ..Default::default()
        })
        .await;
        let session = harness.session.clone();
        let result: std::sync::Arc<std::sync::Mutex<Option<(String, bool)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let sink = std::sync::Arc::clone(&result);
        let target = target_id.clone();
        harness.set_responses(vec![FauxResponseStep::message_factory(move |_input| {
            let session = session.clone();
            let sink = std::sync::Arc::clone(&sink);
            let target = target.clone();
            async move {
                let before = session.session_manager().get_leaf_id().map(str::to_string);
                let outcome = poll_block(session.navigate_tree(target, Default::default()));
                let after = session.session_manager().get_leaf_id().map(str::to_string);
                let error = match outcome {
                    Ok(_) => "ok".to_string(),
                    Err(error) => error.to_string(),
                };
                *sink.lock().unwrap() = Some((error, before == after));
                kiana_ai::providers::faux::faux_assistant_message("response", Default::default())
            }
        })]);
        harness
            .session
            .prompt("second".to_string(), Default::default())
            .await
            .unwrap();
        let recorded = result.lock().unwrap().clone().expect("navigation result");
        assert_eq!(
            recorded.0,
            "Wait for the current response to finish before navigating the session tree."
        );
        assert!(recorded.1);
        harness.cleanup();
    })
    .await;
}
