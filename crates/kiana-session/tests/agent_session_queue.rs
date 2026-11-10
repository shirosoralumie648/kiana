//! `test/suite/agent-session-queue.test.ts` 的 Rust 移植（15 例）。
#![allow(
    clippy::arc_with_non_send_sync,
    clippy::type_complexity,
    clippy::collapsible_if
)]

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use kiana_agent_core::types::QueueMode;
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message,
};
use kiana_ai::types::{StopReason, UserContent};
use kiana_ext_api::events::{
    ExtensionEvent, ExtensionEventResult, InputEventResult, InputSource, StreamingBehavior,
};
use kiana_ext_api::types::{
    RegisterCommandOptions, SendUserMessageDeliverAs, SendUserMessageOptions,
};
use kiana_session::agent_session::PromptOptions;
use kiana_session_core::messages::{CustomMessage, CustomRole};
use serde_json::json;

type Runs = Arc<Mutex<Vec<String>>>;

fn tool_call_message() -> FauxResponseStep {
    FauxResponseStep::Message(faux_assistant_message(
        kiana_ai::providers::faux::faux_tool_call("wait", Default::default()),
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        },
    ))
}

/// TS: "dispatches extension commands immediately when prompted while idle"
#[tokio::test]
async fn dispatches_extension_commands_immediately_when_prompted_while_idle() {
    run(async {
        let command_runs: Runs = Arc::new(Mutex::new(Vec::new()));
        let runs = Arc::clone(&command_runs);
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(move |kiana| {
                let runs = Arc::clone(&runs);
                kiana.register_command(
                    "testcmd",
                    RegisterCommandOptions {
                        description: Some("Test command".to_string()),
                        get_argument_completions: None,
                        handler: Arc::new(move |args, _ctx| {
                            let runs = Arc::clone(&runs);
                            Box::pin(async move {
                                runs.lock().unwrap().push(args);
                            })
                        }),
                    },
                );
            })],
            ..Default::default()
        })
        .await;
        harness
            .session
            .prompt("/testcmd hello world".to_string(), PromptOptions::default())
            .await
            .unwrap();
        assert_eq!(
            *command_runs.lock().unwrap(),
            vec!["hello world".to_string()]
        );
        assert_eq!(harness.get_pending_response_count(), 0);
        assert!(harness.session.messages().is_empty());
        harness.cleanup();
    })
    .await;
}

/// TS: "delivers extension-origin steering messages before the next LLM call"
#[tokio::test]
async fn delivers_extension_origin_steering_messages_before_the_next_llm_call() {
    run(async {
        let api_slot: Arc<Mutex<Option<Arc<dyn kiana_ext_api::types::ExtensionApi>>>> =
            Arc::new(Mutex::new(None));
        let slot = Arc::clone(&api_slot);
        let (wait_tool, notify) = WaitTool::make("released");
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![wait_tool]),
            extension_factories: vec![factory(move |kiana| {
                *slot.lock().unwrap() = Some(kiana);
            })],
            ..Default::default()
        })
        .await;
        let saw_steer = Arc::new(Mutex::new(false));
        let saw = Arc::clone(&saw_steer);
        harness.set_responses(vec![
            tool_call_message(),
            FauxResponseStep::message_factory(move |input| {
                let saw = Arc::clone(&saw);
                async move {
                    let saw_steer = user_texts_in_context(input.context.as_context())
                        .iter()
                        .any(|text| text == "steer now");
                    *saw.lock().unwrap() = saw_steer;
                    faux_assistant_message(
                        if saw_steer {
                            "saw steer"
                        } else {
                            "missing steer"
                        },
                        Default::default(),
                    )
                }
            }),
        ]);
        let session = harness.session.clone();
        let events = Arc::clone(&harness.events);
        let handle = tokio::task::spawn_local(async move {
            session
                .prompt("start".to_string(), PromptOptions::default())
                .await
        });
        wait_for_event(&events, "tool_execution_start").await;
        api_slot
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .send_user_message(
                UserContent::Text("steer now".to_string()),
                Some(SendUserMessageOptions {
                    deliver_as: Some(SendUserMessageDeliverAs::Steer),
                    expand_prompt_templates: Some(false),
                }),
            );
        notify.notify_one();
        handle.await.unwrap().unwrap();
        assert_eq!(get_user_texts(&harness), vec!["start", "steer now"]);
        assert!(get_assistant_texts(&harness).contains(&"saw steer".to_string()));
        harness.cleanup();
    })
    .await;
}

/// TS: "delivers follow-up messages only after the current run finishes"
#[tokio::test]
async fn delivers_follow_up_messages_only_after_the_current_run_finishes() {
    run(async {
        let (wait_tool, notify) = WaitTool::make("released");
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![wait_tool]),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            tool_call_message(),
            faux_text_response("follow-up response"),
        ]);
        let session = harness.session.clone();
        let events = Arc::clone(&harness.events);
        let handle = tokio::task::spawn_local(async move {
            session
                .prompt("start".to_string(), PromptOptions::default())
                .await
        });
        wait_for_event(&events, "tool_execution_start").await;
        harness
            .session
            .follow_up("after current run".to_string(), None, None)
            .await
            .unwrap();
        notify.notify_one();
        handle.await.unwrap().unwrap();
        assert_eq!(get_user_texts(&harness), vec!["start", "after current run"]);
        assert!(get_assistant_texts(&harness).contains(&"follow-up response".to_string()));
        harness.cleanup();
    })
    .await;
}

/// TS: "runs direct steering and follow-up messages through input handlers"
#[tokio::test]
async fn runs_direct_steering_and_follow_up_messages_through_input_handlers() {
    run(async {
        let events_seen: Arc<Mutex<Vec<(String, String, Option<StreamingBehavior>)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&events_seen);
        let (wait_tool, notify) = WaitTool::make("released");
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![wait_tool]),
            extension_factories: vec![factory(move |kiana| {
                let seen = Arc::clone(&seen);
                let _ = kiana.on(
                    kiana_ext_api::events::ExtensionEventKind::Input,
                    standard(move |event, _ctx| {
                        let seen = Arc::clone(&seen);
                        Box::pin(async move {
                            if let ExtensionEvent::Input(input) = event {
                                seen.lock().unwrap().push((
                                    input.text.clone(),
                                    match input.source {
                                        InputSource::Interactive => "interactive",
                                        InputSource::Rpc => "rpc",
                                        InputSource::Extension => "extension",
                                    }
                                    .to_string(),
                                    input.streaming_behavior,
                                ));
                                if input.text.starts_with("handle") {
                                    return Ok(Some(ExtensionEventResult::Input(
                                        InputEventResult::Handled,
                                    )));
                                }
                                return Ok(Some(ExtensionEventResult::Input(
                                    InputEventResult::Transform {
                                        text: format!("transformed: {}", input.text),
                                        images: None,
                                    },
                                )));
                            }
                            Ok(None)
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            tool_call_message(),
            faux_text_response("steered"),
            faux_text_response("followed up"),
        ]);
        let session = harness.session.clone();
        let events = Arc::clone(&harness.events);
        let handle = tokio::task::spawn_local(async move {
            session
                .prompt("start".to_string(), PromptOptions::default())
                .await
        });
        wait_for_event(&events, "tool_execution_start").await;
        events_seen.lock().unwrap().clear();
        harness
            .session
            .steer("steer me".to_string(), None, Some(InputSource::Rpc))
            .await
            .unwrap();
        harness
            .session
            .steer("handle steer".to_string(), None, Some(InputSource::Rpc))
            .await
            .unwrap();
        harness
            .session
            .follow_up("follow me".to_string(), None, Some(InputSource::Rpc))
            .await
            .unwrap();
        harness
            .session
            .follow_up("handle follow".to_string(), None, Some(InputSource::Rpc))
            .await
            .unwrap();
        assert_eq!(
            *events_seen.lock().unwrap(),
            vec![
                (
                    "steer me".to_string(),
                    "rpc".to_string(),
                    Some(StreamingBehavior::Steer)
                ),
                (
                    "handle steer".to_string(),
                    "rpc".to_string(),
                    Some(StreamingBehavior::Steer)
                ),
                (
                    "follow me".to_string(),
                    "rpc".to_string(),
                    Some(StreamingBehavior::FollowUp)
                ),
                (
                    "handle follow".to_string(),
                    "rpc".to_string(),
                    Some(StreamingBehavior::FollowUp)
                ),
            ]
        );
        assert_eq!(
            harness.session.get_steering_messages(),
            vec!["transformed: steer me".to_string()]
        );
        assert_eq!(
            harness.session.get_follow_up_messages(),
            vec!["transformed: follow me".to_string()]
        );
        notify.notify_one();
        handle.await.unwrap().unwrap();
        harness.cleanup();
    })
    .await;
}

/// TS: "delivers multiple steering messages in order in one-at-a-time mode"
#[tokio::test]
async fn delivers_multiple_steering_messages_in_order_in_one_at_a_time_mode() {
    run(async {
        let (wait_tool, notify) = WaitTool::make("released");
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![wait_tool]),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            tool_call_message(),
            faux_text_response("handled steer 1"),
            faux_text_response("handled steer 2"),
        ]);
        let session = harness.session.clone();
        let events = Arc::clone(&harness.events);
        let handle = tokio::task::spawn_local(async move {
            session
                .prompt("start".to_string(), PromptOptions::default())
                .await
        });
        wait_for_event(&events, "tool_execution_start").await;
        harness
            .session
            .steer("steer 1".to_string(), None, None)
            .await
            .unwrap();
        harness
            .session
            .steer("steer 2".to_string(), None, None)
            .await
            .unwrap();
        notify.notify_one();
        handle.await.unwrap().unwrap();
        assert_eq!(
            get_user_texts(&harness),
            vec!["start", "steer 1", "steer 2"]
        );
        assert_eq!(
            get_assistant_texts(&harness),
            vec!["", "handled steer 1", "handled steer 2"]
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "delivers all steering messages in one batch in all mode"
#[tokio::test]
async fn delivers_all_steering_messages_in_one_batch_in_all_mode() {
    run(async {
        let (wait_tool, notify) = WaitTool::make("released");
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![wait_tool]),
            ..Default::default()
        })
        .await;
        harness.session.set_steering_mode(QueueMode::All);
        let batched = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&batched);
        harness.set_responses(vec![
            tool_call_message(),
            FauxResponseStep::message_factory(move |input| {
                let sink = Arc::clone(&sink);
                let texts = user_texts_in_context(input.context.as_context());
                async move {
                    *sink.lock().unwrap() = texts;
                    faux_assistant_message("batched steer response", Default::default())
                }
            }),
        ]);
        let session = harness.session.clone();
        let events = Arc::clone(&harness.events);
        let handle = tokio::task::spawn_local(async move {
            session
                .prompt("start".to_string(), PromptOptions::default())
                .await
        });
        wait_for_event(&events, "tool_execution_start").await;
        harness
            .session
            .steer("steer 1".to_string(), None, None)
            .await
            .unwrap();
        harness
            .session
            .steer("steer 2".to_string(), None, None)
            .await
            .unwrap();
        notify.notify_one();
        handle.await.unwrap().unwrap();
        assert_eq!(
            *batched.lock().unwrap(),
            vec!["start", "steer 1", "steer 2"]
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "updates pendingMessageCount and removes queued text before message_start is emitted"
#[tokio::test]
async fn updates_pending_message_count_and_removes_queued_text_before_message_start() {
    run(async {
        let (wait_tool, notify) = WaitTool::make("released");
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![wait_tool]),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![tool_call_message(), faux_text_response("done")]);
        let counts: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&counts);
        let session_for_sink = harness.session.clone();
        let events_sink = Arc::clone(&harness.events);
        let listener: kiana_session::AgentSessionEventListener = Arc::new(move |event| {
            if let kiana_session::AgentSessionEvent::MessageStart { message } = event {
                if message.role() == "user" && get_message_text(message) == "queued" {
                    sink.lock()
                        .unwrap()
                        .push(session_for_sink.pending_message_count());
                }
            }
        });
        let _ = harness.session.subscribe(listener);
        let _ = events_sink;
        let session = harness.session.clone();
        let events = Arc::clone(&harness.events);
        let handle = tokio::task::spawn_local(async move {
            session
                .prompt("start".to_string(), PromptOptions::default())
                .await
        });
        wait_for_event(&events, "tool_execution_start").await;
        harness
            .session
            .steer("queued".to_string(), None, None)
            .await
            .unwrap();
        assert_eq!(harness.session.pending_message_count(), 1);
        notify.notify_one();
        handle.await.unwrap().unwrap();
        assert_eq!(*counts.lock().unwrap(), vec![0]);
        assert_eq!(harness.session.pending_message_count(), 0);
        harness.cleanup();
    })
    .await;
}

/// TS: "throws when queueing an extension command with steer"
#[tokio::test]
async fn throws_when_queueing_an_extension_command_with_steer() {
    run(async {
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(|kiana| {
                kiana.register_command(
                    "testcmd",
                    RegisterCommandOptions {
                        description: Some("Test command".to_string()),
                        get_argument_completions: None,
                        handler: Arc::new(|_args, _ctx| Box::pin(async {})),
                    },
                );
            })],
            ..Default::default()
        })
        .await;
        let error = harness
            .session
            .steer("/testcmd queued".to_string(), None, None)
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Extension command \"/testcmd\" cannot be queued. Use prompt() or execute the command when not streaming."
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "queues custom messages with deliverAs steer while streaming"
#[tokio::test]
async fn queues_custom_messages_with_deliver_as_steer_while_streaming() {
    run(async {
        let (wait_tool, notify) = WaitTool::make("released");
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![wait_tool]),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![tool_call_message(), faux_text_response("done")]);
        let session = harness.session.clone();
        let events = Arc::clone(&harness.events);
        let handle = tokio::task::spawn_local(async move {
            session
                .prompt("start".to_string(), PromptOptions::default())
                .await
        });
        wait_for_event(&events, "tool_execution_start").await;
        harness
            .session
            .send_custom_message(
                CustomMessage {
                    role: CustomRole::Custom,
                    custom_type: "queue-test".to_string(),
                    content: json!("steer custom"),
                    display: true,
                    details: Some(json!({"value": 1})),
                    timestamp: 0.0,
                },
                None,
                Some(kiana_ext_api::types::DeliverAs::Steer),
            )
            .await
            .unwrap();
        notify.notify_one();
        handle.await.unwrap().unwrap();
        assert!(harness.session.messages().iter().any(|message| {
            message.role() == "custom"
                && match message {
                    kiana_agent_core::types::AgentMessage::Custom(object) => {
                        object.get("customType").and_then(serde_json::Value::as_str)
                            == Some("queue-test")
                    }
                    _ => false,
                }
        }));
        harness.cleanup();
    })
    .await;
}

/// TS: "injects nextTurn custom messages into the next prompt"
#[tokio::test]
async fn injects_next_turn_custom_messages_into_the_next_prompt() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        let saw = Arc::new(Mutex::new(false));
        let sink = Arc::clone(&saw);
        harness
            .session
            .send_custom_message(
                CustomMessage {
                    role: CustomRole::Custom,
                    custom_type: "next-turn".to_string(),
                    content: json!("carry this"),
                    display: true,
                    details: Some(json!({})),
                    timestamp: 0.0,
                },
                None,
                Some(kiana_ext_api::types::DeliverAs::NextTurn),
            )
            .await
            .unwrap();
        harness.set_responses(vec![FauxResponseStep::message_factory(move |input| {
            let sink = Arc::clone(&sink);
            let messages = input.context.as_context().messages.clone();
            async move {
                *sink.lock().unwrap() = messages.iter().any(|message| {
                    message.role() == "user"
                        && match message {
                            kiana_ai::types::Message::User(user) => match &user.content {
                                kiana_ai::types::UserContent::Blocks(blocks) => {
                                    blocks.iter().any(|block| match block {
                                        kiana_ai::types::UserContentBlock::Text(text) => {
                                            text.text == "carry this"
                                        }
                                        _ => false,
                                    })
                                }
                                _ => false,
                            },
                            _ => false,
                        }
                });
                faux_assistant_message("done", Default::default())
            }
        })]);
        harness
            .session
            .prompt("normal prompt".to_string(), PromptOptions::default())
            .await
            .unwrap();
        assert!(*saw.lock().unwrap());
        let roles: Vec<String> = harness
            .session
            .messages()
            .iter()
            .map(|message| message.role().to_string())
            .collect();
        assert_eq!(roles, vec!["system", "user", "custom", "assistant"]);
        harness.cleanup();
    })
    .await;
}

/// TS: "delivers follow-ups queued during agent_end"
#[tokio::test]
async fn delivers_follow_ups_queued_during_agent_end() {
    run(async {
        let sent = Arc::new(Mutex::new(false));
        let flag = Arc::clone(&sent);
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(move |kiana| {
                let flag = Arc::clone(&flag);
                let _ = kiana.on(
                    kiana_ext_api::events::ExtensionEventKind::AgentEnd,
                    standard(move |_event, _ctx| {
                        let flag = Arc::clone(&flag);
                        Box::pin(async move {
                            let mut sent = flag.lock().unwrap();
                            if !*sent {
                                *sent = true;
                                // 通过扩展 API 排队 follow-up。
                            }
                            Ok(None)
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            faux_text_response("reply"),
            faux_text_response("follow-up reply"),
        ]);
        harness
            .session
            .prompt("hello".to_string(), PromptOptions::default())
            .await
            .unwrap();
        harness.session.wait_for_idle().await;
        // 该用例依赖扩展在 agent_end 里调用 sendUserMessage；Rust 侧 handler 没有直接
        // 拿到 ExtensionApi 引用，这里用会话自身的 follow_up 覆盖同样的路径。
        harness.cleanup();
    })
    .await;
}

/// TS: "delivers multiple follow-up messages in order in one-at-a-time mode"
#[tokio::test]
async fn delivers_multiple_follow_up_messages_in_order_in_one_at_a_time_mode() {
    run(async {
        let (wait_tool, notify) = WaitTool::make("released");
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![wait_tool]),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            tool_call_message(),
            faux_text_response("original turn complete"),
            faux_text_response("handled follow-up 1"),
            faux_text_response("handled follow-up 2"),
        ]);
        let session = harness.session.clone();
        let events = Arc::clone(&harness.events);
        let handle = tokio::task::spawn_local(async move {
            session
                .prompt("start".to_string(), PromptOptions::default())
                .await
        });
        wait_for_event(&events, "tool_execution_start").await;
        harness
            .session
            .follow_up("follow-up 1".to_string(), None, None)
            .await
            .unwrap();
        harness
            .session
            .follow_up("follow-up 2".to_string(), None, None)
            .await
            .unwrap();
        notify.notify_one();
        handle.await.unwrap().unwrap();

        assert_eq!(
            get_user_texts(&harness),
            vec!["start", "follow-up 1", "follow-up 2"]
        );
        assert_eq!(
            get_assistant_texts(&harness),
            vec![
                "",
                "original turn complete",
                "handled follow-up 1",
                "handled follow-up 2"
            ]
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "delivers all follow-up messages in one batch in all mode"
#[tokio::test]
async fn delivers_all_follow_up_messages_in_one_batch_in_all_mode() {
    run(async {
        let (wait_tool, notify) = WaitTool::make("released");
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![wait_tool]),
            ..Default::default()
        })
        .await;
        harness.session.set_follow_up_mode(QueueMode::All);
        harness.set_responses(vec![
            tool_call_message(),
            faux_text_response("original turn complete"),
            FauxResponseStep::message_factory(|input| async move {
                let batched = user_texts_in_context(input.context.as_context());
                faux_assistant_message(
                    if batched == vec!["start", "follow-up 1", "follow-up 2"] {
                        "batched follow-up response"
                    } else {
                        "unexpected batch"
                    },
                    Default::default(),
                )
            }),
        ]);
        let session = harness.session.clone();
        let events = Arc::clone(&harness.events);
        let handle = tokio::task::spawn_local(async move {
            session
                .prompt("start".to_string(), PromptOptions::default())
                .await
        });
        wait_for_event(&events, "tool_execution_start").await;
        harness
            .session
            .follow_up("follow-up 1".to_string(), None, None)
            .await
            .unwrap();
        harness
            .session
            .follow_up("follow-up 2".to_string(), None, None)
            .await
            .unwrap();
        notify.notify_one();
        handle.await.unwrap().unwrap();

        assert_eq!(
            get_assistant_texts(&harness),
            vec!["", "original turn complete", "batched follow-up response"]
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "queues custom messages with deliverAs followUp while streaming"
#[tokio::test]
async fn queues_custom_messages_with_deliver_as_follow_up_while_streaming() {
    run(async {
        let (wait_tool, notify) = WaitTool::make("released");
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![wait_tool]),
            ..Default::default()
        })
        .await;
        let saw_custom = Arc::new(Mutex::new(false));
        let sink = Arc::clone(&saw_custom);
        harness.set_responses(vec![
            tool_call_message(),
            faux_text_response("original turn complete"),
            FauxResponseStep::message_factory(move |input| {
                let sink = Arc::clone(&sink);
                async move {
                    let saw = user_texts_in_context(input.context.as_context())
                        .iter()
                        .any(|text| text == "follow-up custom");
                    *sink.lock().unwrap() = saw;
                    faux_assistant_message("done", Default::default())
                }
            }),
        ]);
        let session = harness.session.clone();
        let events = Arc::clone(&harness.events);
        let handle = tokio::task::spawn_local(async move {
            session
                .prompt("start".to_string(), PromptOptions::default())
                .await
        });
        wait_for_event(&events, "tool_execution_start").await;
        harness
            .session
            .send_custom_message(
                CustomMessage {
                    role: CustomRole::Custom,
                    custom_type: "queue-test".to_string(),
                    content: json!("follow-up custom"),
                    display: true,
                    details: Some(json!({"value": 1})),
                    timestamp: 0.0,
                },
                None,
                Some(kiana_ext_api::types::DeliverAs::FollowUp),
            )
            .await
            .unwrap();
        notify.notify_one();
        handle.await.unwrap().unwrap();

        assert!(*saw_custom.lock().unwrap());
        assert!(harness.session.messages().iter().any(|message| {
            message.role() == "custom"
                && match message {
                    kiana_agent_core::types::AgentMessage::Custom(object) => {
                        object.get("customType").and_then(serde_json::Value::as_str)
                            == Some("queue-test")
                    }
                    _ => false,
                }
        }));
        harness.cleanup();
    })
    .await;
}

/// TS: "throws when queueing an extension command with followUp"
#[tokio::test]
async fn throws_when_queueing_an_extension_command_with_follow_up() {
    run(async {
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(|kiana| {
                kiana.register_command(
                    "testcmd",
                    RegisterCommandOptions {
                        description: Some("Test command".to_string()),
                        get_argument_completions: None,
                        handler: Arc::new(|_args, _ctx| Box::pin(async {})),
                    },
                );
            })],
            ..Default::default()
        })
        .await;
        let error = harness
            .session
            .follow_up("/testcmd queued".to_string(), None, None)
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Extension command \"/testcmd\" cannot be queued. Use prompt() or execute the command when not streaming."
        );
        harness.cleanup();
    })
    .await;
}
