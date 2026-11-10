//! `test/suite/regressions/pre-prompt-compaction-no-continue.test.ts` 的 Rust 移植。

mod common;

use common::*;
use kiana_agent_core::types::AgentMessage;
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxModelDefinition, faux_assistant_message,
};
use kiana_ai::types::{StopReason, Usage, UsageCost};
use kiana_compaction::compaction::CompactionResult;
use kiana_ext_api::events::{
    CompactionReason, ExtensionEvent, ExtensionEventKind, ExtensionEventResult,
    SessionBeforeCompactResult, SessionEvent,
};
use kiana_session::agent_session::{AgentSessionEvent, PromptOptions};

fn usage_with_total(total: u64) -> Usage {
    Usage {
        input: total,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: None,
        reasoning: None,
        total_tokens: total,
        cost: UsageCost::default(),
    }
}

fn pre_prompt_summary_extension() -> kiana_ext_api::types::ExtensionFactory {
    factory(|kiana| {
        let _ = kiana.on(
            ExtensionEventKind::SessionBeforeCompact,
            standard(move |event, _ctx| {
                Box::pin(async move {
                    let ExtensionEvent::Session(SessionEvent::BeforeCompact(event)) = event else {
                        return Ok(None);
                    };
                    Ok(Some(ExtensionEventResult::SessionBeforeCompact(
                        SessionBeforeCompactResult {
                            cancel: None,
                            compaction: Some(CompactionResult {
                                summary: "pre-prompt summary".to_string(),
                                first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                                tokens_before: event.preparation.tokens_before,
                                estimated_tokens_after: None,
                                usage: None,
                                details: Some(serde_json::json!({})),
                            }),
                        },
                    )))
                })
            }),
        );
    })
}

/// TS: "compacts length-stop overflow before a new prompt without continuing from an assistant message"
#[tokio::test]
async fn compacts_length_stop_overflow_before_a_new_prompt_without_continuing() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![FauxModelDefinition {
                id: "faux-1".to_string(),
                context_window: Some(100),
                max_tokens: Some(100),
                ..Default::default()
            }]),
            settings: Some(settings_from_json(serde_json::json!({
                "compaction": { "enabled": true, "keepRecentTokens": 1, "reserveTokens": 0 }
            }))),
            extension_factories: vec![pre_prompt_summary_extension()],
            ..Default::default()
        })
        .await;
        let model = harness.get_model(None).expect("model");
        let now = now_ms();
        harness.append_session_message(AgentMessage::Standard(kiana_ai::types::Message::User(
            kiana_ai::types::UserMessage {
                role: kiana_ai::types::UserRole::User,
                content: kiana_ai::types::UserContent::Text("previous prompt".to_string()),
                timestamp: now - 1000,
            },
        )));
        let mut assistant = faux_assistant_message(
            "length-stop assistant response",
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Length),
                timestamp: Some(now - 500),
                ..Default::default()
            },
        );
        assistant.api = model.api.clone();
        assistant.provider = model.provider.clone();
        assistant.model = model.id.clone();
        assistant.usage = usage_with_total(100);
        harness.append_session_message(AgentMessage::Standard(
            kiana_ai::types::Message::Assistant(assistant),
        ));
        harness.set_responses(vec![faux_text_response("answered next prompt")]);

        harness
            .session
            .prompt("next prompt".to_string(), PromptOptions::default())
            .await
            .unwrap();

        let last_end = harness
            .events_of_type("compaction_end")
            .into_iter()
            .next_back()
            .expect("compaction end");
        let AgentSessionEvent::CompactionEnd {
            reason,
            aborted,
            will_retry,
            ..
        } = last_end
        else {
            panic!("unexpected event");
        };
        assert_eq!(reason, CompactionReason::Overflow);
        assert!(!aborted);
        assert!(will_retry);
        assert!(get_user_texts(&harness).contains(&"next prompt".to_string()));
        assert_eq!(harness.faux.state().call_count(), 1);
        harness.cleanup();
    })
    .await;
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
