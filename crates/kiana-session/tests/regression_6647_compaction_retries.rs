//! `test/suite/regressions/6647-compaction-retries-transient-stream-drop.test.ts` 的 Rust 移植。

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::*;
use kiana_agent_core::types::AgentMessage;
use kiana_ai::event_stream::create_assistant_message_event_stream;
use kiana_ai::providers::faux::{FauxAssistantMessageOptions, FauxContent, faux_assistant_message};
use kiana_ai::types::{
    AssistantMessage, AssistantMessageEvent, DoneReason, ErrorReason, Message, StopReason, Usage,
    UsageCost, UserContent, UserContentBlock, UserMessage, UserRole,
};
use kiana_session::agent_session::{AgentSessionEvent, PromptOptions};
use serde_json::json;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn create_usage(total_tokens: u64) -> Usage {
    Usage {
        input: total_tokens,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: None,
        reasoning: None,
        total_tokens,
        cost: UsageCost::default(),
    }
}

fn seed_compactable_session(harness: &Harness) {
    harness
        .settings_manager
        .apply_overrides(settings_from_json(json!({
            "compaction": { "keepRecentTokens": 1 }
        })));
    let now = now_ms();
    harness.append_session_message(AgentMessage::Standard(Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Blocks(vec![UserContentBlock::Text(kiana_ai::types::TextContent {
            kind_text: kiana_ai::types::TextKind::Text,
            text: "message to compact".to_string(),
            text_signature: None,
        })]),
        timestamp: now - 1000,
    })));
    let mut assistant = faux_assistant_message(
        FauxContent::Text(String::new()),
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Stop),
            timestamp: Some(now - 500),
            ..Default::default()
        },
    );
    let model = harness.get_model(None).expect("model");
    assistant.api = model.api.clone();
    assistant.provider = model.provider.clone();
    assistant.model = model.id.clone();
    assistant.usage = create_usage(100);
    assistant.content = vec![kiana_ai::types::AssistantContentBlock::Text(
        kiana_ai::types::TextContent {
            kind_text: kiana_ai::types::TextKind::Text,
            text: "assistant response to compact".to_string(),
            text_signature: None,
        },
    )];
    harness.append_session_message(AgentMessage::Standard(Message::Assistant(assistant)));
}

/// 按脚本返回 assistant 消息的 streamFn；返回调用计数。
fn use_scripted_stream_fn(harness: &Harness, script: Vec<AssistantMessage>) -> Arc<AtomicUsize> {
    let call_count = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&call_count);
    harness
        .agent
        .set_stream_function(Arc::new(move |model, _context, _options| {
            let index = counter.fetch_add(1, Ordering::SeqCst);
            let mut message = script
                .get(index)
                .or_else(|| script.last())
                .cloned()
                .expect("scripted response");
            message.api = model.api.clone();
            message.provider = model.provider.clone();
            message.model = model.id.clone();
            Box::pin(async move {
                let stream = create_assistant_message_event_stream();
                match message.stop_reason {
                    StopReason::Error => {
                        let _ = stream.push(AssistantMessageEvent::Error {
                            reason: ErrorReason::Error,
                            error: Box::new(message.clone()),
                        });
                    }
                    StopReason::Aborted => {
                        let _ = stream.push(AssistantMessageEvent::Error {
                            reason: ErrorReason::Aborted,
                            error: Box::new(message.clone()),
                        });
                    }
                    StopReason::Pending => {
                        let mut error = message.clone();
                        error.stop_reason = StopReason::Error;
                        error.error_message =
                            Some("Scripted response ended without a stop reason".to_string());
                        let _ = stream.push(AssistantMessageEvent::Error {
                            reason: ErrorReason::Error,
                            error: Box::new(error.clone()),
                        });
                        message = error;
                    }
                    _ => {
                        let _ = stream.push(AssistantMessageEvent::Done {
                            reason: DoneReason::Stop,
                            message: Box::new(message.clone()),
                        });
                    }
                }
                stream.end(message);
                Ok(stream)
            })
        }));
    call_count
}

fn error_message(text: &str) -> AssistantMessage {
    let mut message = faux_assistant_message(
        FauxContent::Text(String::new()),
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some(text.to_string()),
            ..Default::default()
        },
    );
    message.usage = create_usage(10);
    message
}

fn success_message(text: &str) -> AssistantMessage {
    let mut message = faux_assistant_message(text, Default::default());
    message.usage = create_usage(10);
    message
}

fn retry_overrides(max_retries: i64, base_delay_ms: i64, enabled: bool) {
    let _ = max_retries;
    let _ = base_delay_ms;
    let _ = enabled;
}

async fn harness_with_retry(max_retries: i64, base_delay_ms: i64) -> Harness {
    let harness = create_harness(HarnessOptions {
        with_configured_auth: Some(false),
        settings: Some(settings_from_json(json!({
            "retry": { "enabled": true, "maxRetries": max_retries, "baseDelayMs": base_delay_ms }
        }))),
        ..Default::default()
    })
    .await;
    seed_compactable_session(&harness);
    harness
}

/// TS: "retries a transient `terminated` summarization error and compacts successfully"
#[tokio::test]
async fn retries_a_transient_terminated_summarization_error() {
    run(async {
        let harness = harness_with_retry(3, 0).await;
        let get_call_count = use_scripted_stream_fn(
            &harness,
            vec![
                error_message("terminated"),
                error_message("terminated"),
                success_message("recovered summary"),
            ],
        );

        let result = harness.session.compact(None).await.unwrap();

        assert!(result.summary.contains("recovered summary"));
        assert_eq!(get_call_count.load(Ordering::SeqCst), 3);
        let starts: Vec<(u64, u64, String)> = harness
            .events_of_type("summarization_retry_scheduled")
            .into_iter()
            .filter_map(|event| match event {
                AgentSessionEvent::SummarizationRetryScheduled {
                    attempt,
                    max_attempts,
                    error_message,
                    ..
                } => Some((attempt, max_attempts, error_message)),
                _ => None,
            })
            .collect();
        assert_eq!(starts.len(), 2);
        assert_eq!(starts[0], (1, 3, "terminated".to_string()));
        assert_eq!(starts[1].0, 2);
        assert_eq!(
            harness.events_of_type("summarization_retry_finished").len(),
            1
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "does not retry a non-retryable error (insufficient_quota)"
#[tokio::test]
async fn does_not_retry_a_non_retryable_error() {
    run(async {
        let harness = harness_with_retry(3, 0).await;
        let get_call_count =
            use_scripted_stream_fn(&harness, vec![error_message("insufficient_quota")]);

        let error = harness.session.compact(None).await.unwrap_err();
        assert!(
            error.to_string().contains("insufficient_quota"),
            "unexpected error: {error}"
        );
        assert_eq!(get_call_count.load(Ordering::SeqCst), 1);
        assert!(
            harness
                .events_of_type("summarization_retry_scheduled")
                .is_empty()
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "does not retry when retry is disabled"
#[tokio::test]
async fn does_not_retry_when_retry_is_disabled() {
    run(async {
        let harness = create_harness(HarnessOptions {
            with_configured_auth: Some(false),
            settings: Some(settings_from_json(json!({
                "retry": { "enabled": false, "maxRetries": 3, "baseDelayMs": 0 }
            }))),
            ..Default::default()
        })
        .await;
        seed_compactable_session(&harness);
        let get_call_count = use_scripted_stream_fn(&harness, vec![error_message("terminated")]);

        let error = harness.session.compact(None).await.unwrap_err();
        assert!(error.to_string().contains("terminated"));
        assert_eq!(get_call_count.load(Ordering::SeqCst), 1);
        assert!(
            harness
                .events_of_type("summarization_retry_scheduled")
                .is_empty()
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "stops retrying after maxRetries and reports failure"
#[tokio::test]
async fn stops_retrying_after_max_retries_and_reports_failure() {
    run(async {
        let harness = harness_with_retry(2, 0).await;
        let get_call_count = use_scripted_stream_fn(
            &harness,
            vec![
                error_message("terminated"),
                error_message("terminated"),
                error_message("terminated"),
            ],
        );

        let error = harness.session.compact(None).await.unwrap_err();
        assert!(error.to_string().contains("terminated"));
        assert_eq!(get_call_count.load(Ordering::SeqCst), 3);
        assert_eq!(
            harness
                .events_of_type("summarization_retry_scheduled")
                .len(),
            2
        );
        assert_eq!(
            harness.events_of_type("summarization_retry_finished").len(),
            1
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "aborts an in-flight retry backoff via abortCompaction"
#[tokio::test]
async fn aborts_an_in_flight_retry_backoff_via_abort_compaction() {
    run(async {
        let harness = harness_with_retry(5, 30_000).await;
        let _ = use_scripted_stream_fn(
            &harness,
            vec![
                error_message("terminated"),
                error_message("terminated"),
                error_message("terminated"),
            ],
        );

        let session = harness.session.clone();
        let compact = tokio::task::spawn_local(async move { session.compact(None).await });
        // 让第一次错误落地、退避 sleep 开始。
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        harness.session.abort_compaction();

        let _ = compact.await.unwrap().unwrap_err();
        match harness
            .events_of_type("compaction_end")
            .into_iter()
            .next_back()
            .expect("compaction_end")
        {
            AgentSessionEvent::CompactionEnd { aborted, .. } => assert!(aborted),
            other => panic!("unexpected event: {other:?}"),
        }
        harness.cleanup();
    })
    .await;
}

// 保持未使用的辅助函数不参与编译告警。
#[allow(dead_code)]
fn _unused() {
    let _ = PromptOptions::default();
    let _: Option<AgentMessage> = None;
    retry_overrides(0, 0, false);
}
