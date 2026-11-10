//! `regressions/3982-message-end-cost-override.test.ts` 的 Rust 移植（regression #3982）。
#![allow(clippy::collapsible_if, clippy::collapsible_match)]

mod common;

use common::*;
use kiana_agent_core::types::AgentMessage;
use kiana_ext_api::events::{
    ExtensionEvent, ExtensionEventKind, ExtensionEventResult, MessageEndEventResult,
};
use kiana_ext_api::types::ExtensionApi;

/// TS: "allows extensions to replace finalized assistant usage cost"
///
/// 忽略原因：`message_end` 替换走 kiana-extensions 的异步派发，派发内部会 `spawn_local`；
/// agent loop 由 `Handle::spawn` 驱动（不在 `LocalSet` 作用域），同线程轮询桥无法驱动
#[tokio::test]
async fn allows_extensions_to_replace_finalized_assistant_usage_cost() {
    run(async {
        let harness = create_harness(HarnessOptions {
            extension_factories: vec![factory(|kiana: std::sync::Arc<dyn ExtensionApi>| {
                let _ = kiana.on(
                    ExtensionEventKind::MessageEnd,
                    standard(move |event, _ctx| {
                        Box::pin(async move {
                            if let ExtensionEvent::MessageEnd(message_end) = event {
                                if let AgentMessage::Standard(
                                    kiana_ai::types::Message::Assistant(assistant),
                                ) = &message_end.message
                                {
                                    let mut replacement = assistant.clone();
                                    replacement.usage.cost.total = 0.123;
                                    return Ok(Some(ExtensionEventResult::MessageEnd(
                                        MessageEndEventResult {
                                            message: Some(AgentMessage::Standard(
                                                kiana_ai::types::Message::Assistant(replacement),
                                            )),
                                        },
                                    )));
                                }
                            }
                            Ok(None)
                        })
                    }),
                );
            })],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![faux_text_response("hello")]);
        harness
            .session
            .prompt("hi".to_string(), Default::default())
            .await
            .unwrap();
        let assistant = harness
            .session
            .messages()
            .into_iter()
            .find_map(|message| match message {
                AgentMessage::Standard(kiana_ai::types::Message::Assistant(assistant)) => {
                    Some(assistant)
                }
                _ => None,
            })
            .expect("assistant message");
        assert_eq!(assistant.usage.cost.total, 0.123);
        let message_end = harness
            .events_of_type("message_end")
            .into_iter()
            .find_map(|event| match event {
                kiana_session::AgentSessionEvent::MessageEnd { message } => match message {
                    AgentMessage::Standard(kiana_ai::types::Message::Assistant(assistant)) => {
                        Some(assistant)
                    }
                    _ => None,
                },
                _ => None,
            })
            .expect("assistant message_end");
        assert_eq!(message_end.usage.cost.total, 0.123);
        harness.cleanup();
    })
    .await;
}
