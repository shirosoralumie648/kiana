//! `test/suite/regressions/2023-queued-slash-command-followup.test.ts` 的 Rust 移植。

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use kiana_ai::types::UserContent;
use kiana_ext_api::types::{
    ExtensionApi, RegisterCommandOptions, SendUserMessageDeliverAs, SendUserMessageOptions,
};
use kiana_session::agent_session::PromptOptions;

/// TS: "treats extension-origin queued slash-command follow-ups as raw user text instead of dispatching the command"
#[tokio::test]
async fn treats_extension_origin_queued_slash_command_follow_ups_as_raw_user_text() {
    run(async {
        let (wait_tool, release) = WaitTool::make("released");
        let command_runs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        #[allow(clippy::arc_with_non_send_sync)] // ExtensionApi 是单线程动作表
        let extension_api: Arc<Mutex<Option<Arc<dyn ExtensionApi>>>> = Arc::new(Mutex::new(None));
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![wait_tool]),
            extension_factories: vec![factory({
                let command_runs = Arc::clone(&command_runs);
                let extension_api = Arc::clone(&extension_api);
                move |kiana| {
                    *extension_api.lock().unwrap() = Some(Arc::clone(&kiana));
                    let command_runs = Arc::clone(&command_runs);
                    kiana.register_command(
                        "testcmd",
                        RegisterCommandOptions {
                            description: Some("Test command".to_string()),
                            get_argument_completions: None,
                            handler: Arc::new(move |args, _ctx| {
                                let command_runs = Arc::clone(&command_runs);
                                Box::pin(async move {
                                    command_runs.lock().unwrap().push(args);
                                })
                            }),
                        },
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            faux_tool_response(
                kiana_ai::providers::faux::faux_tool_call(
                    "wait",
                    kiana_ai::types::JsonObject::new(),
                ),
                kiana_ai::types::StopReason::ToolUse,
            ),
            faux_text_response("first turn complete"),
            faux_text_response("queued follow-up handled by model"),
        ]);

        let prompt_fut = harness
            .session
            .prompt("start".to_string(), PromptOptions::default());
        let queue_fut = async {
            wait_for_event(&harness.events, "tool_execution_start").await;
            tokio::time::sleep(Duration::from_millis(1)).await;
            let api = extension_api
                .lock()
                .unwrap()
                .clone()
                .expect("extension api");
            api.send_user_message(
                UserContent::Text("/testcmd queued".to_string()),
                Some(SendUserMessageOptions {
                    deliver_as: Some(SendUserMessageDeliverAs::FollowUp),
                    expand_prompt_templates: Some(false),
                }),
            );
            release.notify_one();
        };
        let (prompt_result, ()) = futures::join!(prompt_fut, queue_fut);
        prompt_result.unwrap();

        assert!(command_runs.lock().unwrap().is_empty());
        assert_eq!(
            get_user_texts(&harness),
            vec!["start".to_string(), "/testcmd queued".to_string()]
        );
        assert!(
            get_assistant_texts(&harness)
                .iter()
                .any(|text| text == "queued follow-up handled by model")
        );
        harness.cleanup();
    })
    .await;
}
