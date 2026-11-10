//! `test/suite/regressions/8328-zero-usage-auto-compaction.test.ts` 的 Rust 移植。

mod common;

use common::*;
use kiana_agent_core::types::AgentMessage;
use kiana_ai::providers::faux::{FauxModelDefinition, faux_assistant_message};
use kiana_ai::types::{AssistantMessage, StopReason, Usage, UsageCost};
use kiana_ext_api::events::CompactionReason;
use kiana_session::agent_session::AgentSessionEvent;

fn zero_usage() -> Usage {
    Usage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: None,
        reasoning: None,
        total_tokens: 0,
        cost: UsageCost::default(),
    }
}

fn create_zero_usage_assistant(harness: &Harness) -> AssistantMessage {
    let model = harness.get_model(None).expect("model");
    let mut assistant = faux_assistant_message("response", Default::default());
    assistant.api = model.api.clone();
    assistant.provider = model.provider.clone();
    assistant.model = model.id.clone();
    assistant.usage = zero_usage();
    assistant.stop_reason = StopReason::Stop;
    assistant
}

async fn create_compaction_harness() -> Harness {
    create_harness(HarnessOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            context_window: Some(100),
            max_tokens: Some(20),
            ..Default::default()
        }]),
        settings: Some(settings_from_json(serde_json::json!({
            "compaction": { "enabled": true, "keepRecentTokens": 1, "reserveTokens": 10 }
        }))),
        ..Default::default()
    })
    .await
}

fn set_agent_messages(harness: &Harness, user_text: &str, assistant: &AssistantMessage) {
    // 消息既要进 agent 状态，也要在会话分支里（压缩准备按分支切分）。
    harness.append_session_message(AgentMessage::Standard(kiana_ai::types::Message::User(
        kiana_ai::types::UserMessage {
            role: kiana_ai::types::UserRole::User,
            content: kiana_ai::types::UserContent::Text(user_text.to_string()),
            timestamp: 1,
        },
    )));
    harness.append_session_message(AgentMessage::Standard(kiana_ai::types::Message::Assistant(
        assistant.clone(),
    )));
}

/// TS: "uses the message estimate when no assistant has reported usage"
#[tokio::test]
async fn uses_the_message_estimate_when_no_assistant_has_reported_usage() {
    run(async {
        let harness = create_compaction_harness().await;
        let assistant = create_zero_usage_assistant(&harness);
        set_agent_messages(&harness, &"x".repeat(400), &assistant);
        harness.set_responses(vec![faux_text_response("summary")]);
        // `_checkCompaction` 的返回值同 TS：只有重试或排队消息时为 true；这里断言
        // 阈值压缩确实被触发（`compaction_start`）。
        let _ = harness.session.check_compaction_for_test(&assistant).await;
        let starts: Vec<CompactionReason> = harness
            .events_of_type("compaction_start")
            .into_iter()
            .filter_map(|event| match event {
                AgentSessionEvent::CompactionStart { reason } => Some(reason),
                _ => None,
            })
            .collect();
        assert_eq!(starts, vec![CompactionReason::Threshold]);
        harness.cleanup();
    })
    .await;
}

/// TS: "does not compact when the zero-usage message estimate is below the threshold"
#[tokio::test]
async fn does_not_compact_when_the_zero_usage_message_estimate_is_below_the_threshold() {
    run(async {
        let harness = create_compaction_harness().await;
        let assistant = create_zero_usage_assistant(&harness);
        set_agent_messages(&harness, "short", &assistant);
        let compacted = harness.session.check_compaction_for_test(&assistant).await;
        assert!(!compacted);
        assert!(harness.events_of_type("compaction_start").is_empty());
        harness.cleanup();
    })
    .await;
}
