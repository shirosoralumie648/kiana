//! 第 8 周深度接线：context report API 与手动压缩的 CondensationEvent。

mod common;

use common::{HarnessOptions, create_harness, faux_text_response, run, settings_from_json};
use kiana_agent_core::context_report::{
    CondensationConfig, CondensationPhase, ContextReportOptions,
};
use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::{
    AssistantContentBlock, Message, TextContent, TextKind, UserContent, UserMessage, UserRole,
};
use kiana_session::agent_session::{AgentSessionEvent, PromptOptions};
use serde_json::json;

fn seed_two_messages(harness: &common::Harness) {
    harness
        .settings_manager
        .apply_overrides(settings_from_json(json!({
            "compaction": { "keepRecentTokens": 1 }
        })));
    let long_user = "user context line\n".repeat(400);
    harness.append_session_message(AgentMessage::Standard(Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Blocks(vec![kiana_ai::types::UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: long_user,
            text_signature: None,
        })]),
        timestamp: 1,
    })));
    let assistant =
        kiana_ai::providers::faux::faux_assistant_message("assistant response", Default::default());
    let mut assistant = assistant;
    assistant.content = vec![AssistantContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: "assistant response line\n".repeat(400),
        text_signature: None,
    })];
    harness.append_session_message(AgentMessage::Standard(Message::Assistant(assistant)));
}

#[tokio::test]
async fn context_report_api_uses_the_current_context_and_model() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![faux_text_response("hello there")]);
        harness
            .session
            .prompt("say hi".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        let report = harness
            .session
            .context_report(ContextReportOptions::new("[next user prompt]"))
            .expect("context report");

        let model = harness.get_model(None).expect("model");
        assert_eq!(report.model, model.id);
        assert_eq!(report.provider, model.provider);
        assert!(report.total_tokens > 0, "{report:?}");
        assert!(report.request_message_count >= 2, "{report:?}");
        // 顶层 tools 与 system 消息里的 toolsAdded 都会进入工具分段（估算器的
        // usage 锚点只统计后者），因此分段合计可能大于总量；仍应覆盖总量。
        assert!(
            report.tool_tokens
                + report.prompt_tokens
                + report.turn_context_tokens
                + report.segment_tokens
                >= report.total_tokens,
            "categories must cover the total: {report:?}"
        );
        assert!(report.context_window.is_some());
        harness.cleanup();
    })
    .await;
}

/// Phase 4：Context Shadow 是 `context_report` 的只读投影，不修改会话状态；
/// Enforce 未实现，`is_enforced()` 必须恒为 false。
#[tokio::test]
async fn context_shadow_previews_without_enforcing() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![faux_text_response("hello there")]);
        harness
            .session
            .prompt("say hi".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        let shadow = harness
            .session
            .context_shadow(ContextReportOptions::new("[next user prompt]"))
            .expect("context shadow");
        assert!(!shadow.is_enforced());
        assert_eq!(shadow.total_tokens(), shadow.report().total_tokens);
        assert!(!shadow.segments().is_empty());
        assert!(shadow.diff(&shadow).is_empty());

        // 捕获影子不会追加消息：同一选项再取一次报告，消息数不变。
        let again = harness
            .session
            .context_report(ContextReportOptions::new("[next user prompt]"))
            .expect("context report");
        assert_eq!(
            again.request_message_count,
            shadow.report().request_message_count
        );

        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn manual_compaction_emits_condensation_with_matching_saved_tokens() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        seed_two_messages(&harness);
        harness.set_responses(vec![faux_text_response("summary text")]);

        let before = harness.session.context_tokens();
        let result = harness
            .session
            .compact_with_condensation_options(
                None,
                Some(CondensationConfig {
                    settle_ms: 1,
                    timeout_ms: 2_000,
                    poll_interval_ms: 1,
                    min_saved_tokens: 1,
                }),
            )
            .await
            .expect("compact");
        let after = harness.session.context_tokens();
        assert!(
            after < before,
            "compaction must reduce tokens: {before} -> {after}"
        );
        assert!(result.estimated_tokens_after.is_some());

        let events = harness.events_of_type("condensation");
        assert_eq!(
            events.len(),
            1,
            "one condensation event expected: {events:?}"
        );
        let AgentSessionEvent::Condensation { event } = &events[0] else {
            panic!("unexpected event: {:?}", events[0]);
        };
        assert_eq!(event.phase, CondensationPhase::Settled, "{event:?}");
        assert_eq!(event.before_tokens, before);
        assert_eq!(event.after_tokens, after);
        assert_eq!(event.saved_tokens, before - after);
        assert!(event.error.is_none());

        let last = harness
            .session
            .last_condensation()
            .expect("last condensation is retained");
        assert_eq!(&last, event);
        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn manual_compaction_failure_emits_failed_condensation() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        // 没有任何可压缩的历史：compact() 返回 NothingToCompact。
        let error = harness.session.compact(None).await.unwrap_err();
        assert!(
            error.to_string().contains("Nothing to compact")
                || error.to_string().contains("Already compacted"),
            "unexpected error: {error}"
        );
        let events = harness.events_of_type("condensation");
        assert_eq!(events.len(), 1);
        let AgentSessionEvent::Condensation { event } = &events[0] else {
            panic!("unexpected event: {:?}", events[0]);
        };
        assert_eq!(event.phase, CondensationPhase::Failed);
        assert!(event.error.is_some());
        harness.cleanup();
    })
    .await;
}
