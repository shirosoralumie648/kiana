//! `test/suite/regressions/7048-compaction-truncated-summary.test.ts` 的 Rust 移植。

mod common;

use common::*;
use kiana_agent_core::types::AgentMessage;
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message,
};
use kiana_ai::types::{AssistantContentBlock, StopReason, Usage, UsageCost};
use kiana_session_core::session_manager::SessionEntry;

fn seeded_usage() -> Usage {
    Usage {
        input: 100,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: None,
        reasoning: None,
        total_tokens: 100,
        cost: UsageCost::default(),
    }
}

fn seed_compactable_session(harness: &Harness) {
    harness
        .settings_manager
        .apply_overrides(settings_from_json(serde_json::json!({
            "compaction": { "keepRecentTokens": 1 }
        })));
    let now = now_ms();
    harness.append_session_message(AgentMessage::Standard(kiana_ai::types::Message::User(
        kiana_ai::types::UserMessage {
            role: kiana_ai::types::UserRole::User,
            content: kiana_ai::types::UserContent::Blocks(vec![
                kiana_ai::types::UserContentBlock::Text(kiana_ai::types::TextContent {
                    kind_text: kiana_ai::types::TextKind::Text,
                    text: "message to compact".to_string(),
                    text_signature: None,
                }),
            ]),
            timestamp: now - 1000,
        },
    )));
    let model = harness.get_model(None).expect("model");
    let mut assistant = faux_assistant_message(
        AssistantContentBlock::Text(kiana_ai::types::TextContent {
            kind_text: kiana_ai::types::TextKind::Text,
            text: "assistant response to compact".to_string(),
            text_signature: None,
        }),
        FauxAssistantMessageOptions {
            timestamp: Some(now - 500),
            ..Default::default()
        },
    );
    assistant.api = model.api.clone();
    assistant.provider = model.provider.clone();
    assistant.model = model.id.clone();
    assistant.usage = seeded_usage();
    harness.append_session_message(AgentMessage::Standard(kiana_ai::types::Message::Assistant(
        assistant,
    )));
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// TS: "does not persist a length-limited summary"
#[tokio::test]
async fn does_not_persist_a_length_limited_summary() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        seed_compactable_session(&harness);
        harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            "partial summar",
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Length),
                ..Default::default()
            },
        ))]);
        let error = harness.session.compact(None).await.unwrap_err();
        assert!(
            error.to_string().contains("generation hit the token cap"),
            "unexpected error: {error}"
        );
        let compactions = harness
            .session_entries()
            .into_iter()
            .filter(|entry| matches!(entry, SessionEntry::Compaction(_)))
            .count();
        assert_eq!(compactions, 0);
        harness.cleanup();
    })
    .await;
}
