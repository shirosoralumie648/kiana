//! 第 9 周：输入预检的零请求保证与 run summary 的 canonical 费用一致性。

mod common;

use common::*;
use kiana_ai::providers::faux::FauxModelDefinition;
use kiana_ai::types::ModelCost;
use kiana_session::agent_session::{AgentSessionEvent, PromptOptions};
use serde_json::json;

/// 非 faux provider（独立 mock provider）超窗时：provider 一次请求都不发，错误文本是
/// typed `ContextWindowExceeded`。
#[tokio::test]
async fn oversized_input_fails_with_zero_provider_requests() {
    run(async {
        let harness = create_harness(HarnessOptions {
            provider: Some("mock".to_string()),
            models: Some(vec![FauxModelDefinition {
                id: "mock-1".to_string(),
                context_window: Some(100),
                max_tokens: Some(16),
                ..Default::default()
            }]),
            settings: Some(settings_from_json(json!({
                "permissions": {"mode": "bypass"},
                "compaction": {"enabled": false}
            }))),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![faux_text_response("must not be consumed")]);
        harness
            .session
            .prompt("x".repeat(4_000), PromptOptions::default())
            .await
            .expect("the run fails via an error assistant message, not a prompt error");

        assert_eq!(
            harness.get_pending_response_count(),
            1,
            "the provider must not receive any request"
        );
        let errors: Vec<String> = harness
            .events_of_type("message_end")
            .into_iter()
            .filter_map(|event| match event {
                AgentSessionEvent::MessageEnd { message } => match message {
                    kiana_agent_core::types::AgentMessage::Standard(
                        kiana_ai::types::Message::Assistant(assistant),
                    ) if assistant.stop_reason == kiana_ai::types::StopReason::Error => {
                        assistant.error_message
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert!(
            errors
                .iter()
                .any(|error| error.contains("context window") || error.contains("context length")),
            "typed precheck error must reach the assistant message: {errors:?}"
        );
        harness.cleanup();
    })
    .await;
}

/// run summary 的 cost 使用与 SessionTelemetry 相同的 canonical 记账：
/// provider 上报 cost=0 但模型目录有价格时，summary 仍给出目录价金额。
#[tokio::test]
async fn run_summary_cost_uses_canonical_pricing() {
    run(async {
        let harness = create_harness(HarnessOptions {
            models: Some(vec![FauxModelDefinition {
                id: "priced-1".to_string(),
                cost: Some(ModelCost {
                    input: 3.0,
                    output: 15.0,
                    cache_read: 0.3,
                    cache_write: 3.75,
                    tiers: None,
                }),
                ..Default::default()
            }]),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![faux_text_response("priced")]);
        harness
            .session
            .prompt("hello".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        let summary = harness.session.run_summary().expect("run summary");
        assert_eq!(summary.chats.total, 1);
        let usage = &summary.usage;
        let raw_input = usage
            .input_tokens
            .saturating_sub(usage.cached_input_tokens)
            .saturating_sub(usage.cache_write_tokens);
        let expected = (raw_input as f64 * 3.0
            + usage.output_tokens as f64 * 15.0
            + usage.cached_input_tokens as f64 * 0.3
            + usage.cache_write_tokens as f64 * 3.75)
            / 1_000_000.0;
        assert!(expected > 0.0, "faux usage must contain tokens");
        assert!(
            (summary.cost.estimated_usd - expected).abs() < 1e-12,
            "summary cost {} != canonical {expected}",
            summary.cost.estimated_usd
        );
        harness.cleanup();
    })
    .await;
}
