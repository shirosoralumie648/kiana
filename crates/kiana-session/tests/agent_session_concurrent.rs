//! `test/agent-session-concurrent.test.ts` 的 Rust 移植（验收门补齐）。
//!
//! TS 用自定义 streamFn 模拟「流中挂起」；这里用 `WaitTool`（工具执行期间保持流式）达到同样
//! 的可观察状态。用例 6/7（tool_call 事件等待 message_end 结算、慢 handler 的持久化顺序）
//! 由 `regression_1717_2113_event_settlement.rs` 的两个同名断言覆盖。

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use common::*;
use kiana_ai::types::UserContent;
use kiana_ext_api::events::{
    ExtensionEvent, ExtensionEventKind, ExtensionEventResult, InputEventResult, InputSource,
};
use kiana_ext_api::types::{SendUserMessageDeliverAs, SendUserMessageOptions};
use kiana_session::agent_session::{AgentSessionEvent, PromptOptions, QueuedInputDisposition};

/// 启动一个会阻塞在 `wait` 工具上的 prompt（返回 join handle）。
fn start_blocking_prompt(
    harness: &Harness,
    text: &str,
) -> tokio::task::JoinHandle<Result<(), kiana_session::agent_session::AgentSessionError>> {
    let session = harness.session.clone();
    let text = text.to_string();
    tokio::task::spawn_local(async move { session.prompt(text, PromptOptions::default()).await })
}

fn wait_responses() -> Vec<kiana_ai::providers::faux::FauxResponseStep> {
    vec![
        faux_tool_response(
            kiana_ai::providers::faux::faux_tool_call("wait", Default::default()),
            kiana_ai::types::StopReason::ToolUse,
        ),
        faux_text_response("done"),
    ]
}

/// TS: "should throw when prompt() called while streaming"
#[tokio::test]
async fn throws_when_prompt_called_while_streaming() {
    run(async {
        let (wait_tool, notify) = WaitTool::make("released");
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![wait_tool]),
            ..Default::default()
        })
        .await;
        harness.set_responses(wait_responses());
        let first_prompt = start_blocking_prompt(&harness, "First message");
        wait_for_event(&harness.events, "tool_execution_start").await;

        assert!(harness.session.is_streaming());
        let error = harness
            .session
            .prompt("Second message".to_string(), PromptOptions::default())
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message."
        );

        notify.notify_one();
        harness.session.abort().await;
        let _ = first_prompt.await;
        harness.cleanup();
    })
    .await;
}

/// TS: "should allow steer() while streaming"
#[tokio::test]
async fn allows_steer_while_streaming() {
    run(async {
        let (wait_tool, notify) = WaitTool::make("released");
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![wait_tool]),
            ..Default::default()
        })
        .await;
        harness.set_responses(wait_responses());
        let first_prompt = start_blocking_prompt(&harness, "First message");
        wait_for_event(&harness.events, "tool_execution_start").await;

        let disposition = harness
            .session
            .steer("Steering message".to_string(), None, None)
            .await
            .unwrap();
        assert_eq!(disposition, QueuedInputDisposition::Queued);
        assert_eq!(harness.session.pending_message_count(), 1);

        notify.notify_one();
        harness.session.abort().await;
        let _ = first_prompt.await;
        harness.cleanup();
    })
    .await;
}

/// TS: "should allow followUp() while streaming"
#[tokio::test]
async fn allows_follow_up_while_streaming() {
    run(async {
        let (wait_tool, notify) = WaitTool::make("released");
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![wait_tool]),
            ..Default::default()
        })
        .await;
        harness.set_responses(wait_responses());
        let first_prompt = start_blocking_prompt(&harness, "First message");
        wait_for_event(&harness.events, "tool_execution_start").await;

        let disposition = harness
            .session
            .follow_up("Follow-up message".to_string(), None, None)
            .await
            .unwrap();
        assert_eq!(disposition, QueuedInputDisposition::Queued);
        assert_eq!(harness.session.pending_message_count(), 1);

        notify.notify_one();
        harness.session.abort().await;
        let _ = first_prompt.await;
        harness.cleanup();
    })
    .await;
}

/// TS: "should queue extension-origin steering messages while streaming"
#[tokio::test]
async fn queues_extension_origin_steering_messages_while_streaming() {
    run(async {
        let sent = Arc::new(AtomicBool::new(false));
        let input_sources: Arc<Mutex<Vec<InputSource>>> = Arc::new(Mutex::new(Vec::new()));
        let sources = Arc::clone(&input_sources);
        let send_flag = Arc::clone(&sent);
        let (wait_tool, notify) = WaitTool::make("released");
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![wait_tool]),
            extension_factories: vec![
                factory(move |kiana| {
                    let send_flag = Arc::clone(&send_flag);
                    let api = Arc::clone(&kiana);
                    let _ = kiana.on(
                        ExtensionEventKind::ToolExecutionStart,
                        standard(move |_event, _ctx| {
                            let kiana = Arc::clone(&api);
                            let send_flag = Arc::clone(&send_flag);
                            Box::pin(async move {
                                if !send_flag.swap(true, Ordering::SeqCst) {
                                    kiana.send_user_message(
                                        UserContent::Text("Steer from extension".to_string()),
                                        Some(SendUserMessageOptions {
                                            deliver_as: Some(SendUserMessageDeliverAs::Steer),
                                            expand_prompt_templates: None,
                                        }),
                                    );
                                }
                                Ok(None)
                            })
                        }),
                    );
                }),
                factory(move |kiana| {
                    let sources = Arc::clone(&sources);
                    let _ = kiana.on(
                        ExtensionEventKind::Input,
                        standard(move |event, _ctx| {
                            let sources = Arc::clone(&sources);
                            Box::pin(async move {
                                if let ExtensionEvent::Input(input) = event {
                                    sources.lock().unwrap().push(input.source);
                                }
                                Ok(Some(ExtensionEventResult::Input(
                                    InputEventResult::Continue,
                                )))
                            })
                        }),
                    );
                }),
            ],
            ..Default::default()
        })
        .await;
        harness.set_responses(wait_responses());
        let first_prompt = start_blocking_prompt(&harness, "First message");
        wait_for_event(&harness.events, "tool_execution_start").await;

        // 等扩展的 steering 消息入队。
        for _ in 0..2000 {
            if harness.session.pending_message_count() == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        assert_eq!(harness.session.pending_message_count(), 1);
        assert!(
            harness
                .session
                .get_steering_messages()
                .iter()
                .any(|text| text == "Steer from extension")
        );
        assert!(
            input_sources
                .lock()
                .unwrap()
                .contains(&InputSource::Extension)
        );
        let queue_updates: Vec<Vec<String>> = harness
            .events_of_type("queue_update")
            .into_iter()
            .filter_map(|event| match event {
                AgentSessionEvent::QueueUpdate { steering, .. } => Some(steering),
                _ => None,
            })
            .collect();
        assert!(
            queue_updates
                .iter()
                .any(|steering| steering.iter().any(|text| text == "Steer from extension"))
        );

        notify.notify_one();
        harness.session.abort().await;
        let _ = first_prompt.await;
        harness.cleanup();
    })
    .await;
}

/// TS: "should allow prompt() after previous completes"
#[tokio::test]
async fn allows_prompt_after_previous_completes() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![
            faux_text_response("Done"),
            faux_text_response("Done again"),
        ]);

        harness
            .session
            .prompt("First message".to_string(), PromptOptions::default())
            .await
            .unwrap();
        assert!(!harness.session.is_streaming());
        harness
            .session
            .prompt("Second message".to_string(), PromptOptions::default())
            .await
            .unwrap();

        let user_messages = harness
            .session
            .messages()
            .into_iter()
            .filter(|message| {
                matches!(
                    message,
                    kiana_agent_core::types::AgentMessage::Standard(
                        kiana_ai::types::Message::User(_)
                    )
                )
            })
            .count();
        assert_eq!(user_messages, 2);
        harness.cleanup();
    })
    .await;
}
