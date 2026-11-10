//! `test/suite/agent-session-retry-events.test.ts` 的 Rust 移植（4 例）。
//! 其余重试用例见 `agent_session_retry.rs` 与 `agent_session_retry_events_extra.rs`。
#![allow(clippy::collapsible_if)]

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use kiana_ai::providers::faux::{FauxAssistantMessageOptions, faux_assistant_message};
use kiana_ai::types::StopReason;
use kiana_session::agent_session::{AgentSessionEvent, PromptOptions};
use serde_json::json;

fn error_response(message: &str) -> kiana_ai::providers::faux::FauxResponseStep {
    kiana_ai::providers::faux::FauxResponseStep::Message(faux_assistant_message(
        "",
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error),
            error_message: Some(message.to_string()),
            ..Default::default()
        },
    ))
}

fn retry_settings(
    max_retries: i64,
    base_delay_ms: i64,
) -> kiana_config::settings::schema::Settings {
    settings_from_json(json!({
        "retry": {
            "enabled": true,
            "maxRetries": max_retries,
            "baseDelayMs": base_delay_ms,
        }
    }))
}

/// TS: "retries after a transient error and succeeds"
#[tokio::test]
async fn retries_after_a_transient_error_and_succeeds() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(retry_settings(3, 1)),
            ..Default::default()
        })
        .await;
        let retry_events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&retry_events);
        let _ = harness
            .session
            .subscribe(Arc::new(move |event| match event {
                AgentSessionEvent::AutoRetryStart { attempt, .. } => {
                    sink.lock().unwrap().push(format!("start:{attempt}"));
                }
                AgentSessionEvent::AutoRetryEnd { success, .. } => {
                    sink.lock().unwrap().push(format!("end:{success}"));
                }
                _ => {}
            }));
        harness.set_responses(vec![
            error_response("overloaded_error"),
            faux_text_response("recovered"),
        ]);
        harness
            .session
            .prompt("test".to_string(), PromptOptions::default())
            .await
            .unwrap();
        assert_eq!(
            *retry_events.lock().unwrap(),
            vec!["start:1".to_string(), "end:true".to_string()]
        );
        let will_retry: Vec<bool> = harness
            .events_of_type("agent_end")
            .into_iter()
            .filter_map(|event| match event {
                AgentSessionEvent::AgentEnd { will_retry, .. } => Some(will_retry),
                _ => None,
            })
            .collect();
        assert_eq!(will_retry, vec![true, false]);
        assert_eq!(harness.faux.state().call_count(), 2);
        assert!(!harness.session.is_retrying());
        harness.cleanup();
    })
    .await;
}

/// TS: "retries multiple transient failures and succeeds on the final attempt"
#[tokio::test]
async fn retries_multiple_transient_failures_and_succeeds_on_the_final_attempt() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(retry_settings(3, 1)),
            ..Default::default()
        })
        .await;
        let retry_events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&retry_events);
        let _ = harness
            .session
            .subscribe(Arc::new(move |event| match event {
                AgentSessionEvent::AutoRetryStart { attempt, .. } => {
                    sink.lock().unwrap().push(format!("start:{attempt}"));
                }
                AgentSessionEvent::AutoRetryEnd { success, .. } => {
                    sink.lock().unwrap().push(format!("end:{success}"));
                }
                _ => {}
            }));
        harness.set_responses(vec![
            error_response("overloaded_error"),
            error_response("overloaded_error"),
            faux_text_response("success"),
        ]);
        harness
            .session
            .prompt("test".to_string(), PromptOptions::default())
            .await
            .unwrap();
        assert_eq!(
            *retry_events.lock().unwrap(),
            vec![
                "start:1".to_string(),
                "start:2".to_string(),
                "end:true".to_string()
            ]
        );
        assert_eq!(harness.faux.state().call_count(), 3);
        harness.cleanup();
    })
    .await;
}

/// TS: "exhausts max retries and emits a failure event"
#[tokio::test]
async fn exhausts_max_retries_and_emits_a_failure_event() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(retry_settings(2, 1)),
            ..Default::default()
        })
        .await;
        let retry_events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&retry_events);
        let _ = harness
            .session
            .subscribe(Arc::new(move |event| match event {
                AgentSessionEvent::AutoRetryStart { attempt, .. } => {
                    sink.lock().unwrap().push(format!("start:{attempt}"));
                }
                AgentSessionEvent::AutoRetryEnd { success, .. } => {
                    sink.lock().unwrap().push(format!("end:{success}"));
                }
                _ => {}
            }));
        harness.set_responses(vec![
            error_response("overloaded_error"),
            error_response("overloaded_error"),
            error_response("overloaded_error"),
        ]);
        harness
            .session
            .prompt("test".to_string(), PromptOptions::default())
            .await
            .unwrap();
        assert_eq!(
            *retry_events.lock().unwrap(),
            vec![
                "start:1".to_string(),
                "start:2".to_string(),
                "end:false".to_string()
            ]
        );
        let will_retry: Vec<bool> = harness
            .events_of_type("agent_end")
            .into_iter()
            .filter_map(|event| match event {
                AgentSessionEvent::AgentEnd { will_retry, .. } => Some(will_retry),
                _ => None,
            })
            .collect();
        assert_eq!(will_retry, vec![true, true, false]);
        assert_eq!(harness.faux.state().call_count(), 3);
        assert!(!harness.session.is_retrying());
        harness.cleanup();
    })
    .await;
}

/// TS: "finalizes retry state when abort is requested after a retry attempt fails"
#[tokio::test]
async fn finalizes_retry_state_when_abort_is_requested_after_a_retry_attempt_fails() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(retry_settings(3, 0)),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            error_response("overloaded_error"),
            error_response("overloaded_error"),
        ]);
        let error_count = Arc::new(Mutex::new(0u32));
        let counter = Arc::clone(&error_count);
        let session = harness.session.clone();
        let _ = harness.session.subscribe(Arc::new(move |event| {
            if let AgentSessionEvent::MessageEnd { message } = event
                && let Some(assistant) = message.as_assistant()
                && assistant.stop_reason == StopReason::Error
            {
                let mut count = counter.lock().unwrap();
                *count += 1;
                if *count == 2 {
                    let session = session.clone();
                    std::mem::drop(count);
                    session.request_abort();
                }
            }
        }));
        harness
            .session
            .prompt("test".to_string(), PromptOptions::default())
            .await
            .unwrap();
        assert_eq!(harness.session.retry_attempt(), 0);
        let last_agent_end = harness.events_of_type("agent_end").pop();
        assert!(matches!(
            last_agent_end,
            Some(AgentSessionEvent::AgentEnd {
                will_retry: false,
                ..
            })
        ));
        let last_retry_end = harness.events_of_type("auto_retry_end").into_iter().last();
        assert!(matches!(
            last_retry_end,
            Some(AgentSessionEvent::AutoRetryEnd {
                success: false,
                attempt: 1,
                final_error: Some(ref error),
            }) if error == "Retry cancelled"
        ));
        harness.cleanup();
    })
    .await;
}
