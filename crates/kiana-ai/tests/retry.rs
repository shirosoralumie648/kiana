//! `packages/ai/test/retry.test.ts`。
//!
//! TS 用 `vi.fn()` 统计调用次数与回调参数；Rust 侧用 `Arc<Mutex<Vec<...>>>` 记录同样的事件，
//! 并用 `tokio::test` 驱动 `retry_assistant_call`。TS 的 `fauxAssistantMessage` 是 provider
//! 测试助手（不在本单元），这里直接用 [`AssistantMessage`] 构造同形的消息。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiana_ai::types::{AssistantContentBlock, AssistantMessage, StopReason, TextContent, TextKind};
use kiana_ai::utils::abort::AbortScope;
use kiana_ai::utils::retry::{
    RetryCallbacks, RetryPolicy, is_retryable_assistant_error, retry_assistant_call, retry_delay_ms,
};

const OPENAI_EXPLICIT_RETRY_MESSAGE: &str = "An error occurred while processing your request. You can retry your request, or contact us through our help center at help.openai.com if the error persists. Please include the request ID req_******** in your message.";
const BEDROCK_EXPLICIT_RETRY_MESSAGE: &str = r#"{"message":"The system encountered an unexpected error during processing. Try your request again."}"#;
const NVIDIA_NIM_RESOURCE_EXHAUSTED_MESSAGE: &str =
    "ResourceExhausted: Worker local total request limit reached (288/48)";
const BUN_FETCH_SOCKET_CLOSED_MESSAGE: &str = "The socket connection was closed unexpectedly. For more information, pass `verbose: true` in the second argument to fetch()";
const OPENAI_RESPONSES_EARLY_EOF_MESSAGE: &str =
    "OpenAI Responses stream ended before a terminal response event";
const WRAPPED_DNS_LOOKUP_ERROR: &str = "The pending stream has been canceled (caused by: getaddrinfo ENOTFOUND bedrock-runtime.us-east-1.amazonaws.com)";
const AZURE_PEAK_LOAD_ERROR: &str = "The system is currently experiencing high demand and cannot process your request. Your request exceeds the maximum usage size allowed during peak load. For improved capacity reliability, consider switching to Provisioned Throughput.";

fn text_message(text: &str) -> AssistantMessage {
    AssistantMessage {
        content: vec![AssistantContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: text.into(),
            text_signature: None,
        })],
        stop_reason: StopReason::Stop,
        ..Default::default()
    }
}

fn error_message(message: &str) -> AssistantMessage {
    AssistantMessage {
        stop_reason: StopReason::Error,
        error_message: Some(message.to_string()),
        ..Default::default()
    }
}

fn aborted_message() -> AssistantMessage {
    AssistantMessage {
        stop_reason: StopReason::Aborted,
        ..Default::default()
    }
}

fn retryable(message: &str) -> bool {
    is_retryable_assistant_error(&error_message(message))
}

// describe("provider retry classification")

#[test]
fn matches_explicit_provider_retry_guidance() {
    assert!(retryable(OPENAI_EXPLICIT_RETRY_MESSAGE));
    assert!(retryable(BEDROCK_EXPLICIT_RETRY_MESSAGE));
    assert!(retryable(NVIDIA_NIM_RESOURCE_EXHAUSTED_MESSAGE));
}

#[test]
fn matches_bun_fetch_socket_drop_wording() {
    assert!(retryable(BUN_FETCH_SOCKET_CLOSED_MESSAGE));
}

#[test]
fn matches_upstream_request_buffer_exhaustion_wording() {
    assert!(retryable(
        "Error: exceeded request buffer limit while retrying upstream"
    ));
}

#[test]
fn matches_dns_transport_failure_wording() {
    for message in [
        WRAPPED_DNS_LOOKUP_ERROR,
        "connect ENOTFOUND api.example.com",
        "EAI_AGAIN api.example.com",
        "getaddrinfo failed for api.example.com",
    ] {
        assert!(retryable(message), "should retry: {message}");
    }
}

#[test]
fn matches_http_2_pending_stream_cancellation() {
    // Regression for #10379.
    for message in [
        "The pending stream has been canceled",
        "The pending stream has been canceled (caused by: socket closed)",
    ] {
        assert!(retryable(message), "should retry: {message}");
    }
}

#[test]
fn matches_service_unavailable_wording() {
    for message in [
        // Regression for #10965 (provider unknown, no status code in the message).
        "The service is temporarily unavailable. Please retry later.",
        // Bedrock ServiceUnavailableException as formatted by formatBedrockError.
        "Service unavailable: The service is temporarily unavailable.",
    ] {
        assert!(retryable(message), "should retry: {message}");
    }
}

#[test]
fn matches_node_fetch_request_failure_wording() {
    // Regression for #10996: node-fetch errors from the google-vertex OAuth token refresh.
    for message in [
        "request to https://oauth2.googleapis.com/token failed, reason:",
        "request to https://oauth2.googleapis.com/token failed, reason: connect ECONNREFUSED 127.0.0.1:9",
    ] {
        assert!(retryable(message), "should retry: {message}");
    }
}

#[test]
fn matches_gateway_upstream_failure_wording() {
    // Regression for #10920: gateway upstream failures sent as `response.failed` error codes.
    for message in [
        "unknown: Unable to forward upstream stream",
        "upstream_error: Upstream request failed",
        "upstream_stream_read_error: Upstream response stream was interrupted",
        "Upstream HTTP/2 stream failed",
    ] {
        assert!(retryable(message), "should retry: {message}");
    }
}

#[test]
fn matches_openai_responses_streams_that_end_before_terminal_events() {
    assert!(retryable(OPENAI_RESPONSES_EARLY_EOF_MESSAGE));
}

#[test]
fn matches_azure_peak_load_capacity_errors() {
    // Regression for #9669.
    assert!(retryable(AZURE_PEAK_LOAD_ERROR));
}

#[test]
fn keeps_provider_limit_errors_non_retryable() {
    assert!(!retryable("429 quota exceeded"));
}

#[test]
fn keeps_the_chatgpt_subscription_usage_limit_non_retryable() {
    let error_message = r#"OpenAI API error (429): {"code":"subscription_sharing_usage_limit_exceeded","message":"Usage limit reached."}"#;
    assert!(!retryable(error_message));
}

#[test]
fn keeps_the_chatgpt_business_workspace_usage_limit_non_retryable() {
    let error_message = r#"OpenAI API error (429): {"code":"subscription_sharing_workplace_individual_usage_limit_exceeded","message":"Usage limit reached."}"#;
    assert!(!retryable(error_message));
}

#[test]
fn retries_temporary_chatgpt_subscription_errors() {
    for error_message in [
        "subscription_sharing_usage_unavailable: Usage cannot be checked.",
        "subscription_sharing_user_unavailable: User cannot be loaded.",
    ] {
        assert!(retryable(error_message), "should retry: {error_message}");
    }
}

#[test]
fn classifies_assistant_error_messages() {
    assert!(retryable("overloaded_error"));
    // Regression for #9627.
    assert!(retryable("520 status code (no body)"));
    assert!(retryable("524 status code (no body)"));
    assert!(!is_retryable_assistant_error(&text_message("not an error")));
}

#[test]
fn does_not_match_status_codes_inside_ids_or_longer_numbers() {
    // Regression for #11057: status codes inside ids or other numbers are not status codes.
    for error_message in [
        r#"401 {"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"},"request_id":"req_011CU503xyz"}"#,
        "400 image exceeds 5 MB maximum: 5316784 bytes > 5242880 bytes",
    ] {
        assert!(
            !retryable(error_message),
            "should not retry: {error_message}"
        );
    }
}

// describe("retryDelayMs")

#[test]
fn caps_agent_retry_delay() {
    // Regression for #8826.
    assert_eq!(retry_delay_ms(2000, None, 6), 60_000);
    assert_eq!(retry_delay_ms(2000, Some(5000), 5), 5000);
    assert_eq!(retry_delay_ms(2000, Some(0), 5), 0);
}

// ---------------------------------------------------------------------------
// describe("retryAssistantCall")
// ---------------------------------------------------------------------------

/// `vi.fn()` 等价物：按脚本依次返回消息并记录调用次数。
struct Producer {
    calls: Arc<AtomicUsize>,
    script: Vec<AssistantMessage>,
    fallback: AssistantMessage,
}

impl Producer {
    fn new(script: Vec<AssistantMessage>) -> (Self, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let fallback = script.last().cloned().unwrap_or_default();
        (
            Producer {
                calls: Arc::clone(&calls),
                script,
                fallback,
            },
            calls,
        )
    }

    fn next(&self) -> AssistantMessage {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        self.script
            .get(index)
            .cloned()
            .unwrap_or_else(|| self.fallback.clone())
    }
}

#[derive(Debug, Default, PartialEq, Clone)]
struct RetryEvents {
    scheduled: Vec<(u64, u64, u64, String)>,
    attempt_starts: usize,
    finished: Vec<(bool, u64, Option<String>)>,
}

fn callbacks(events: Arc<Mutex<RetryEvents>>) -> RetryCallbacks {
    RetryCallbacks {
        on_retry_scheduled: Some(Arc::new({
            let events = Arc::clone(&events);
            move |attempt, max, delay, message| {
                events
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .scheduled
                    .push((attempt, max, delay, message));
                Box::pin(async {})
            }
        })),
        on_retry_attempt_start: Some(Arc::new({
            let events = Arc::clone(&events);
            move || {
                events
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .attempt_starts += 1;
                Box::pin(async {})
            }
        })),
        on_retry_finished: Some(Arc::new({
            let events = Arc::clone(&events);
            move |success, attempts, message| {
                events
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .finished
                    .push((success, attempts, message));
                Box::pin(async {})
            }
        })),
    }
}

fn snapshot(events: &Arc<Mutex<RetryEvents>>) -> RetryEvents {
    (*events.lock().unwrap_or_else(|error| error.into_inner())).clone()
}

fn enabled() -> RetryPolicy {
    RetryPolicy {
        enabled: true,
        max_retries: 3,
        base_delay_ms: 0,
        max_agent_delay_ms: None,
    }
}

fn disabled() -> RetryPolicy {
    RetryPolicy {
        enabled: false,
        max_retries: 3,
        base_delay_ms: 0,
        max_agent_delay_ms: None,
    }
}

#[tokio::test]
async fn returns_a_successful_response_immediately_without_retrying() {
    let (producer, calls) = Producer::new(vec![text_message("ok")]);
    let result =
        retry_assistant_call(|| async { producer.next() }, Some(&enabled()), None, None).await;
    assert_eq!(result.content.len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn does_not_retry_an_aborted_message() {
    let (producer, calls) = Producer::new(vec![aborted_message()]);
    let events = Arc::new(Mutex::new(RetryEvents::default()));
    let result = retry_assistant_call(
        || async { producer.next() },
        Some(&enabled()),
        None,
        Some(&callbacks(Arc::clone(&events))),
    )
    .await;
    assert_eq!(result.stop_reason, StopReason::Aborted);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(snapshot(&events).scheduled.is_empty());
}

#[tokio::test]
async fn does_not_retry_a_non_retryable_error_quota_billing() {
    let (producer, calls) = Producer::new(vec![error_message("insufficient_quota")]);
    let events = Arc::new(Mutex::new(RetryEvents::default()));
    let result = retry_assistant_call(
        || async { producer.next() },
        Some(&enabled()),
        None,
        Some(&callbacks(Arc::clone(&events))),
    )
    .await;
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let recorded = snapshot(&events);
    assert!(recorded.scheduled.is_empty());
    assert!(recorded.finished.is_empty());
}

#[tokio::test]
async fn retries_a_transient_error_up_to_maxretries_then_returns_the_final_error() {
    let (producer, calls) = Producer::new(vec![error_message("terminated")]);
    let events = Arc::new(Mutex::new(RetryEvents::default()));
    let result = retry_assistant_call(
        || async { producer.next() },
        Some(&enabled()),
        None,
        Some(&callbacks(Arc::clone(&events))),
    )
    .await;
    assert_eq!(result.stop_reason, StopReason::Error);
    // 1 initial + 3 retries
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    let recorded = snapshot(&events);
    assert_eq!(recorded.scheduled.len(), 3);
    assert_eq!(
        recorded.finished,
        vec![(false, 3, Some("terminated".to_string()))]
    );
}

#[tokio::test]
async fn reports_capped_retry_delays() {
    // Regression for #8826.
    let calls = Arc::new(AtomicUsize::new(0));
    let policy = RetryPolicy {
        enabled: true,
        max_retries: 4,
        base_delay_ms: 10,
        max_agent_delay_ms: Some(15),
    };
    let produce = {
        let calls = Arc::clone(&calls);
        move || {
            let calls = Arc::clone(&calls);
            async move {
                let count = calls.fetch_add(1, Ordering::SeqCst) + 1;
                if count < 5 {
                    error_message("terminated")
                } else {
                    text_message("recovered")
                }
            }
        }
    };
    let events = Arc::new(Mutex::new(RetryEvents::default()));
    let result = retry_assistant_call(
        produce,
        Some(&policy),
        None,
        Some(&callbacks(Arc::clone(&events))),
    )
    .await;
    assert_eq!(result.content.len(), 1);
    let delays: Vec<u64> = snapshot(&events)
        .scheduled
        .iter()
        .map(|call| call.2)
        .collect();
    assert_eq!(delays, vec![10, 15, 15, 15]);
}

#[tokio::test]
async fn stops_retrying_once_a_call_succeeds() {
    let calls = Arc::new(AtomicUsize::new(0));
    let produce = {
        let calls = Arc::clone(&calls);
        move || {
            let calls = Arc::clone(&calls);
            async move {
                let count = calls.fetch_add(1, Ordering::SeqCst) + 1;
                if count < 3 {
                    error_message("terminated")
                } else {
                    text_message("recovered")
                }
            }
        }
    };
    let events = Arc::new(Mutex::new(RetryEvents::default()));
    let result = retry_assistant_call(
        produce,
        Some(&enabled()),
        None,
        Some(&callbacks(Arc::clone(&events))),
    )
    .await;
    assert_eq!(result.content.len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(snapshot(&events).finished, vec![(true, 2, None)]);
}

#[tokio::test]
async fn reports_an_aborted_retried_call_as_unsuccessful() {
    let calls = Arc::new(AtomicUsize::new(0));
    let produce = {
        let calls = Arc::clone(&calls);
        move || {
            let calls = Arc::clone(&calls);
            async move {
                let count = calls.fetch_add(1, Ordering::SeqCst) + 1;
                if count == 1 {
                    error_message("terminated")
                } else {
                    aborted_message()
                }
            }
        }
    };
    let events = Arc::new(Mutex::new(RetryEvents::default()));
    let result = retry_assistant_call(
        produce,
        Some(&enabled()),
        None,
        Some(&callbacks(Arc::clone(&events))),
    )
    .await;
    assert_eq!(result.stop_reason, StopReason::Aborted);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(snapshot(&events).finished, vec![(false, 1, None)]);
}

#[tokio::test]
async fn does_not_retry_when_policy_is_disabled() {
    let (producer, calls) = Producer::new(vec![error_message("terminated")]);
    let events = Arc::new(Mutex::new(RetryEvents::default()));
    let result = retry_assistant_call(
        || async { producer.next() },
        Some(&disabled()),
        None,
        Some(&callbacks(Arc::clone(&events))),
    )
    .await;
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let recorded = snapshot(&events);
    assert!(recorded.scheduled.is_empty());
    assert!(recorded.finished.is_empty());
}

#[tokio::test]
async fn emits_onretryattemptstart_after_backoff_before_each_retried_call() {
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let produce = {
        let calls = Arc::clone(&calls);
        let events = Arc::clone(&events);
        move || {
            let calls = Arc::clone(&calls);
            let events = Arc::clone(&events);
            async move {
                let count = calls.fetch_add(1, Ordering::SeqCst);
                events
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(format!("produce:{count}"));
                if count + 1 < 3 {
                    error_message("terminated")
                } else {
                    text_message("recovered")
                }
            }
        }
    };
    let callbacks = RetryCallbacks {
        on_retry_scheduled: Some(Arc::new({
            let events = Arc::clone(&events);
            move |attempt, _, _, _| {
                events
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(format!("retry:{attempt}"));
                Box::pin(async {})
            }
        })),
        on_retry_attempt_start: Some(Arc::new({
            let events = Arc::clone(&events);
            move || {
                events
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push("attempt-start".to_string());
                Box::pin(async {})
            }
        })),
        on_retry_finished: None,
    };
    let result = retry_assistant_call(produce, Some(&enabled()), None, Some(&callbacks)).await;
    assert_eq!(result.content.len(), 1);
    assert_eq!(
        *events.lock().unwrap_or_else(|error| error.into_inner()),
        vec![
            "produce:0",
            "retry:1",
            "attempt-start",
            "produce:1",
            "retry:2",
            "attempt-start",
            "produce:2",
        ]
    );
}

#[tokio::test]
async fn aborts_backoff_sleep_via_signal_returns_an_aborted_message_and_emits_onretryfinishedfalse()
{
    let scope = AbortScope::new();
    let (producer, calls) = Producer::new(vec![error_message("terminated")]);
    let policy = RetryPolicy {
        enabled: true,
        max_retries: 5,
        base_delay_ms: 10_000,
        max_agent_delay_ms: None,
    };
    let events = Arc::new(Mutex::new(RetryEvents::default()));
    let aborter = {
        let scope = scope.clone();
        tokio::spawn(async move {
            // 等第一次调用进入退避睡眠后再中止（对应 TS 的 `vi.waitFor(produce called)`）。
            tokio::time::sleep(Duration::from_millis(50)).await;
            scope.abort();
        })
    };
    let result = retry_assistant_call(
        || async { producer.next() },
        Some(&policy),
        Some(&scope),
        Some(&callbacks(Arc::clone(&events))),
    )
    .await;
    aborter.await.expect("aborter task");
    assert_eq!(result.stop_reason, StopReason::Aborted);
    assert_eq!(result.error_message, None);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        snapshot(&events).finished,
        vec![(false, 1, Some("terminated".to_string()))]
    );
}
