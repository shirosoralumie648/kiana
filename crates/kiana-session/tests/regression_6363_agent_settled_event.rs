//! `test/suite/regressions/6363-agent-settled-event.test.ts` 的 Rust 移植。

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use common::*;
use kiana_ext_api::events::{ExtensionEvent, ExtensionEventKind};
use kiana_ext_api::types::{
    ExtensionCommandContextActions, NewSessionResult, RegisterCommandOptions,
};
use kiana_session::agent_session::{AgentSessionEvent, ExtensionBindings, PromptOptions};
use tokio::sync::Notify;

/// TS: "emits one agent_settled event after automatic retry finishes"
#[tokio::test]
async fn emits_one_agent_settled_event_after_automatic_retry_finishes() {
    run(async {
        let extension_events: Arc<std::sync::Mutex<Vec<String>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_from_json(serde_json::json!({
                "retry": { "enabled": true, "maxRetries": 3, "baseDelayMs": 1 }
            }))),
            extension_factories: vec![factory({
                let extension_events = Arc::clone(&extension_events);
                move |kiana| {
                    let events = Arc::clone(&extension_events);
                    let _ = kiana.on(
                        ExtensionEventKind::AgentEnd,
                        standard(move |_event, _ctx| {
                            let events = Arc::clone(&events);
                            Box::pin(async move {
                                events.lock().unwrap().push("agent_end".to_string());
                                Ok(None)
                            })
                        }),
                    );
                    let events = Arc::clone(&extension_events);
                    let _ = kiana.on(
                        ExtensionEventKind::AgentSettled,
                        standard(move |event, ctx| {
                            let events = Arc::clone(&events);
                            Box::pin(async move {
                                let ExtensionEvent::AgentSettled(_) = event else {
                                    return Ok(None);
                                };
                                events
                                    .lock()
                                    .unwrap()
                                    .push(format!("agent_settled:{}", ctx.is_idle()));
                                Ok(None)
                            })
                        }),
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            kiana_ai::providers::faux::FauxResponseStep::Message(
                kiana_ai::providers::faux::faux_assistant_message(
                    "",
                    kiana_ai::providers::faux::FauxAssistantMessageOptions {
                        stop_reason: Some(kiana_ai::types::StopReason::Error),
                        error_message: Some("overloaded_error".to_string()),
                        ..Default::default()
                    },
                ),
            ),
            faux_text_response("recovered"),
        ]);

        harness
            .session
            .prompt("test".to_string(), PromptOptions::default())
            .await
            .unwrap();

        let will_retry: Vec<bool> = harness
            .events_of_type("agent_end")
            .into_iter()
            .filter_map(|event| match event {
                AgentSessionEvent::AgentEnd { will_retry, .. } => Some(will_retry),
                _ => None,
            })
            .collect();
        assert_eq!(will_retry, vec![true, false]);
        let settled: Vec<bool> = harness
            .events_of_type("agent_settled")
            .into_iter()
            .filter_map(|event| match event {
                AgentSessionEvent::AgentSettled { aborted } => Some(aborted),
                _ => None,
            })
            .collect();
        assert_eq!(settled, vec![false]);
        assert_eq!(
            *extension_events.lock().unwrap(),
            vec![
                "agent_end".to_string(),
                "agent_end".to_string(),
                "agent_settled:true".to_string()
            ]
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "settles only after follow-ups queued by agent_end handlers run"
#[tokio::test]
async fn settles_only_after_follow_ups_queued_by_agent_end_handlers_run() {
    run(async {
        let queued = Arc::new(AtomicBool::new(false));
        let settled_idle_states: Arc<std::sync::Mutex<Vec<bool>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory({
                let queued = Arc::clone(&queued);
                let settled_idle_states = Arc::clone(&settled_idle_states);
                move |kiana| {
                    let api = Arc::clone(&kiana);
                    let queued_flag = Arc::clone(&queued);
                    let _ = kiana.on(
                        ExtensionEventKind::AgentEnd,
                        standard(move |_event, _ctx| {
                            let api = Arc::clone(&api);
                            let queued = Arc::clone(&queued_flag);
                            Box::pin(async move {
                                if queued.swap(true, Ordering::SeqCst) {
                                    return Ok(None);
                                }
                                api.send_user_message(
                                    kiana_ai::types::UserContent::Text("status follow-up".to_string()),
                                    Some(kiana_ext_api::types::SendUserMessageOptions {
                                        deliver_as: Some(
                                            kiana_ext_api::types::SendUserMessageDeliverAs::FollowUp,
                                        ),
                                        expand_prompt_templates: Some(false),
                                    }),
                                );
                                Ok(None)
                            })
                        }),
                    );
                    let states = Arc::clone(&settled_idle_states);
                    let _ = kiana.on(
                        ExtensionEventKind::AgentSettled,
                        standard(move |_event, ctx| {
                            let states = Arc::clone(&states);
                            Box::pin(async move {
                                states.lock().unwrap().push(ctx.is_idle());
                                Ok(None)
                            })
                        }),
                    );
                }
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            faux_text_response("first"),
            faux_text_response("second"),
        ]);

        harness
            .session
            .prompt("hello".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(
            get_user_texts(&harness),
            vec!["hello".to_string(), "status follow-up".to_string()]
        );
        assert_eq!(harness.events_of_type("agent_end").len(), 2);
        assert_eq!(harness.events_of_type("agent_settled").len(), 1);
        assert_eq!(*settled_idle_states.lock().unwrap(), vec![true]);
        harness.cleanup();
    })
    .await;
}

/// TS: "extension command waitForIdle waits for session-level settlement"
#[tokio::test]
async fn extension_command_wait_for_idle_waits_for_session_level_settlement() {
    run(async {
        let (wait_tool, tool_release) = WaitTool::make("released");
        let command_started = Arc::new(Notify::new());
        let command_finished = Arc::new(AtomicBool::new(false));
        let command_results: Arc<std::sync::Mutex<Vec<bool>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![wait_tool]),
            extension_factories: vec![factory({
                let command_started = Arc::clone(&command_started);
                let command_finished = Arc::clone(&command_finished);
                let command_results = Arc::clone(&command_results);
                move |kiana| {
                    let started = Arc::clone(&command_started);
                    let finished = Arc::clone(&command_finished);
                    let results = Arc::clone(&command_results);
                    kiana.register_command(
                        "after-idle",
                        RegisterCommandOptions {
                            description: Some("Wait for idle".to_string()),
                            get_argument_completions: None,
                            handler: Arc::new(move |_args, ctx| {
                                let started = Arc::clone(&started);
                                let finished = Arc::clone(&finished);
                                let results = Arc::clone(&results);
                                Box::pin(async move {
                                    started.notify_one();
                                    ctx.wait_for_idle().await;
                                    results.lock().unwrap().push(ctx.is_idle());
                                    finished.store(true, Ordering::SeqCst);
                                })
                            }),
                        },
                    );
                }
            })],
            ..Default::default()
        })
        .await;

        let session_for_idle = harness.session.clone();
        let bindings = ExtensionBindings {
            command_context_actions: Some(ExtensionCommandContextActions {
                wait_for_idle: Arc::new(move || {
                    let session = session_for_idle.clone();
                    Box::pin(async move { session.wait_for_idle().await })
                }),
                new_session: Arc::new(|_options| {
                    Box::pin(async { NewSessionResult { cancelled: false } })
                }),
                fork: Arc::new(|_entry_id, _options| {
                    Box::pin(async { NewSessionResult { cancelled: false } })
                }),
                navigate_tree: Arc::new(|_target_id, _options| {
                    Box::pin(async { NewSessionResult { cancelled: false } })
                }),
                switch_session: Arc::new(|_path, _options| {
                    Box::pin(async { NewSessionResult { cancelled: false } })
                }),
                reload: Arc::new(|| Box::pin(async {})),
            }),
            ..Default::default()
        };
        harness.session.bind_extensions(bindings).await;

        harness.set_responses(vec![
            kiana_ai::providers::faux::FauxResponseStep::Message(
                kiana_ai::providers::faux::faux_assistant_message(
                    kiana_ai::providers::faux::faux_tool_call(
                        "wait",
                        kiana_ai::types::JsonObject::new(),
                    ),
                    kiana_ai::providers::faux::FauxAssistantMessageOptions {
                        stop_reason: Some(kiana_ai::types::StopReason::ToolUse),
                        ..Default::default()
                    },
                ),
            ),
            faux_text_response("done"),
        ]);

        let prompt_fut = harness
            .session
            .prompt("start".to_string(), PromptOptions::default());
        let command_fut = async {
            wait_for_event(&harness.events, "tool_execution_start").await;
            let command = harness
                .session
                .prompt("/after-idle".to_string(), PromptOptions::default());
            let watcher = async {
                command_started.notified().await;
                tokio::time::sleep(Duration::from_millis(5)).await;
                assert!(!command_finished.load(Ordering::SeqCst));
                tool_release.notify_one();
            };
            let (result, ()) = futures::join!(command, watcher);
            result
        };
        let (prompt_result, command_result) = futures::join!(prompt_fut, command_fut);
        prompt_result.unwrap();
        command_result.unwrap();

        assert_eq!(*command_results.lock().unwrap(), vec![true]);
        assert_eq!(harness.events_of_type("agent_settled").len(), 1);
        harness.cleanup();
    })
    .await;
}
