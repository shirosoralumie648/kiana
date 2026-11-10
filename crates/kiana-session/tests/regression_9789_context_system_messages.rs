//! `test/suite/regressions/9789-context-handler-system-messages.test.ts` 的 Rust 移植。
//!
//! 未移植：
//! - "keeps mid-conversation system messages in place ..."：TS 通过就地修改
//!   `event.systemPromptOptions.sections` 影响后续请求；Rust 的事件按值传递，没有等价对象身份。
//! - "applies in-place edits to event.messages without a return value"：TS 依赖 JS 数组的就地
//!   splice；Rust 的 Standard handler 收到的是 JSON 化副本，就地修改不会回传。

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use kiana_agent_core::types::{AgentMessage, AgentTool};
use kiana_ai::providers::faux::{FauxResponseStep, faux_assistant_message, faux_tool_call};
use kiana_ai::types::{Message, StopReason, TextContent, TextKind, Tool, UserContentBlock};
use kiana_ext_api::events::{
    ContextEventResult, ExtensionEvent, ExtensionEventKind, ExtensionEventResult,
    SessionBeforeCompactResult,
};
use kiana_session::agent_session::{AgentSessionEvent, PromptOptions};
use serde_json::json;

fn compact_via_hook(kiana: Arc<dyn kiana_ext_api::types::ExtensionApi>) {
    let _ = kiana.on(
        ExtensionEventKind::SessionBeforeCompact,
        standard(|event, _ctx| {
            Box::pin(async move {
                let ExtensionEvent::Session(kiana_ext_api::events::SessionEvent::BeforeCompact(
                    before,
                )) = event
                else {
                    return Ok(None);
                };
                Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                    SessionBeforeCompactResult {
                        cancel: None,
                        compaction: Some(kiana_compaction::compaction::CompactionResult {
                            summary: "extension summary".to_string(),
                            first_kept_entry_id: before.preparation.first_kept_entry_id.clone(),
                            tokens_before: before.preparation.tokens_before,
                            estimated_tokens_after: None,
                            usage: None,
                            details: Some(json!({"source": "test"})),
                        }),
                    },
                )))
            })
        }),
    );
}

type Captured = Arc<Mutex<Option<kiana_ai::types::Context>>>;

fn capture_request(harness: &Harness, text: &str) -> Captured {
    let captured: Captured = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&captured);
    let text = text.to_string();
    harness.set_responses(vec![FauxResponseStep::message_factory(move |input| {
        let sink = Arc::clone(&sink);
        let context = input.context.as_context().clone();
        let text = text.clone();
        async move {
            *sink.lock().unwrap() = Some(context);
            faux_assistant_message(text, Default::default())
        }
    })]);
    captured
}

fn request(_harness: &Harness, captured: &Captured) -> kiana_ai::types::Context {
    captured
        .lock()
        .unwrap()
        .clone()
        .expect("expected a provider request")
}

fn tool_names(messages: &[Message]) -> Vec<String> {
    kiana_ai::utils::transcript::get_current_tools(messages)
        .into_iter()
        .map(|tool| tool.name)
        .collect()
}

fn role_names(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .map(|message| message.role().to_string())
        .collect()
}

async fn compact_session(harness: &Harness) {
    harness
        .settings_manager
        .apply_overrides(settings_from_json(json!({
            "compaction": { "keepRecentTokens": 1 }
        })));
    harness.set_responses(vec![faux_text_response("one"), faux_text_response("two")]);
    harness
        .session
        .prompt("first".to_string(), PromptOptions::default())
        .await
        .unwrap();
    harness
        .session
        .prompt("second".to_string(), PromptOptions::default())
        .await
        .unwrap();
    harness.session.compact(None).await.unwrap();
    let roles: Vec<String> = harness
        .session
        .messages()
        .iter()
        .take(2)
        .map(|message| message.role().to_string())
        .collect();
    assert_eq!(roles, vec!["system", "compactionSummary"]);
}

/// TS: "keeps the prompt and tools when a handler slices from the compaction summary"
#[tokio::test]
async fn keeps_prompt_and_tools_when_handler_slices_from_compaction_summary() {
    run(async {
        let seen: Arc<Mutex<Vec<Vec<AgentMessage>>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![
                factory(compact_via_hook),
                factory(move |kiana| {
                    let sink = Arc::clone(&sink);
                    let _ = kiana.on(
                        ExtensionEventKind::Context,
                        standard(move |event, _ctx| {
                            let sink = Arc::clone(&sink);
                            Box::pin(async move {
                                let ExtensionEvent::Context(context) = event else {
                                    return Ok(None);
                                };
                                sink.lock().unwrap().push(context.messages.clone());
                                let index = context
                                    .messages
                                    .iter()
                                    .position(|message| message.role() == "compactionSummary")
                                    .unwrap_or(0);
                                Ok(Some(ExtensionEventResult::Context(ContextEventResult {
                                    messages: Some(context.messages[index..].to_vec()),
                                })))
                            })
                        }),
                    );
                }),
            ],
            ..Default::default()
        })
        .await;
        compact_session(&harness).await;
        let captured = capture_request(&harness, "after compaction");

        harness
            .session
            .prompt("third".to_string(), PromptOptions::default())
            .await
            .unwrap();

        let seen = seen.lock().unwrap();
        assert!(
            !seen
                .last()
                .unwrap()
                .iter()
                .any(|message| message.role() == "system")
        );
        let request = request(&harness, &captured);
        assert_eq!(request.messages[0].role(), "system");
        assert_eq!(
            tool_names(&request.messages),
            harness.session.get_active_tool_names()
        );
        assert_eq!(
            kiana_ai::utils::transcript::get_current_system_prompt(&request.messages),
            harness.session.system_prompt()
        );
        assert_eq!(
            request
                .messages
                .iter()
                .filter(|message| message.role() == "system")
                .count(),
            1
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "keeps system messages a handler adds after the replayed head"
#[tokio::test]
async fn keeps_system_messages_added_after_the_replayed_head() {
    run(async {
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(|kiana| {
                let _ = kiana.on(
                    ExtensionEventKind::Context,
                    standard(|event, _ctx| {
                        Box::pin(async move {
                            let ExtensionEvent::Context(context) = event else {
                                return Ok(None);
                            };
                            let mut messages = vec![AgentMessage::Standard(Message::System(
                                kiana_ai::types::SystemMessage {
                                    role: kiana_ai::types::SystemRole::System,
                                    content: kiana_ai::types::SystemContent::Text(
                                        "ephemeral reminder".to_string(),
                                    ),
                                    sections: None,
                                    tools_added: None,
                                    tools_removed: None,
                                    timestamp: 0,
                                },
                            ))];
                            messages.extend(context.messages);
                            Ok(Some(ExtensionEventResult::Context(ContextEventResult {
                                messages: Some(messages),
                            })))
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        let captured = capture_request(&harness, "done");

        harness
            .session
            .prompt("hello".to_string(), PromptOptions::default())
            .await
            .unwrap();

        let request = request(&harness, &captured);
        assert_eq!(
            role_names(&request.messages),
            vec!["system", "system", "user"]
        );
        assert_eq!(
            tool_names(&request.messages),
            harness.session.get_active_tool_names()
        );
        let prompt = kiana_ai::utils::transcript::get_current_system_prompt(&request.messages);
        assert!(prompt.contains(&harness.session.system_prompt()));
        assert!(prompt.contains("ephemeral reminder"));
        harness.cleanup();
    })
    .await;
}

/// TS: "runs after context handlers on the restored transcript and sends its output verbatim"
#[tokio::test]
async fn runs_context_with_system_after_context_handlers() {
    run(async {
        let seen: Arc<Mutex<Vec<Vec<AgentMessage>>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![
                factory(compact_via_hook),
                factory(move |kiana| {
                    let sink = Arc::clone(&sink);
                    // 先注册 context_with_system，后注册 context；TS 规定 context 先跑。
                    let _ = kiana.on(
                        ExtensionEventKind::ContextWithSystem,
                        standard(move |event, _ctx| {
                            let sink = Arc::clone(&sink);
                            Box::pin(async move {
                                let ExtensionEvent::ContextWithSystem(context) = event else {
                                    return Ok(None);
                                };
                                sink.lock().unwrap().push(context.messages.clone());
                                let messages = context
                                    .messages
                                    .into_iter()
                                    .map(|message| match message {
                                        AgentMessage::Standard(Message::System(mut system))
                                            if system.tools_added.is_some() =>
                                        {
                                            system.tools_added = Some(
                                                system
                                                    .tools_added
                                                    .unwrap_or_default()
                                                    .into_iter()
                                                    .filter(|tool: &Tool| tool.name != "bash")
                                                    .collect(),
                                            );
                                            AgentMessage::Standard(Message::System(system))
                                        }
                                        other => other,
                                    })
                                    .collect();
                                Ok(Some(ExtensionEventResult::Context(ContextEventResult {
                                    messages: Some(messages),
                                })))
                            })
                        }),
                    );
                    let _ = kiana.on(
                        ExtensionEventKind::Context,
                        standard(|event, _ctx| {
                            Box::pin(async move {
                                let ExtensionEvent::Context(context) = event else {
                                    return Ok(None);
                                };
                                let index = context
                                    .messages
                                    .iter()
                                    .position(|message| message.role() == "compactionSummary")
                                    .unwrap_or(0);
                                Ok(Some(ExtensionEventResult::Context(ContextEventResult {
                                    messages: Some(context.messages[index..].to_vec()),
                                })))
                            })
                        }),
                    );
                }),
            ],
            ..Default::default()
        })
        .await;
        compact_session(&harness).await;
        let captured = capture_request(&harness, "after compaction");

        harness
            .session
            .prompt("third".to_string(), PromptOptions::default())
            .await
            .unwrap();

        let seen = seen.lock().unwrap();
        let input = seen.last().expect("context_with_system input");
        assert_eq!(input[0].role(), "system");
        assert_eq!(input[1].role(), "compactionSummary");
        assert!(
            harness
                .session
                .get_active_tool_names()
                .iter()
                .any(|name| name == "bash")
        );
        let expected: Vec<String> = harness
            .session
            .get_active_tool_names()
            .into_iter()
            .filter(|name| name != "bash")
            .collect();
        assert_eq!(tool_names(&request(&harness, &captured).messages), expected);
        harness.cleanup();
    })
    .await;
}

/// TS: "reports a handler that drops the leading system message but honors its output"
#[tokio::test]
async fn reports_handler_that_drops_leading_system_message() {
    run(async {
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(|kiana| {
                let _ = kiana.on(
                    ExtensionEventKind::ContextWithSystem,
                    standard(|event, _ctx| {
                        Box::pin(async move {
                            let ExtensionEvent::ContextWithSystem(context) = event else {
                                return Ok(None);
                            };
                            Ok(Some(ExtensionEventResult::Context(ContextEventResult {
                                messages: Some(
                                    context
                                        .messages
                                        .into_iter()
                                        .filter(|message| message.role() != "system")
                                        .collect(),
                                ),
                            })))
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&errors);
        let _unsubscribe = harness
            .session
            .extension_runner()
            .on_error(Arc::new(move |error| {
                sink.lock()
                    .unwrap()
                    .push(format!("{}: {}", error.event, error.error));
            }));
        let captured = capture_request(&harness, "done");

        harness
            .session
            .prompt("hello".to_string(), PromptOptions::default())
            .await
            .unwrap();

        assert_eq!(
            role_names(&request(&harness, &captured).messages),
            vec!["user"]
        );
        let errors = errors.lock().unwrap();
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0]
                .starts_with("context_with_system: Handler removed the leading system message"),
            "unexpected error: {}",
            errors[0]
        );
        harness.cleanup();
    })
    .await;
}

// 保持未使用的导入不产生告警。
#[allow(dead_code)]
fn _unused(
    _: AgentTool,
    _: Option<AgentSessionEvent>,
    _: TextContent,
    _: TextKind,
    _: UserContentBlock,
) {
    let _ = StopReason::Stop;
    let _ = faux_tool_call("x", indexmap::IndexMap::new());
}
