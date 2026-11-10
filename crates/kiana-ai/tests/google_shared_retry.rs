//! `test/google-shared-retry.test.ts` 的移植。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use kiana_ai::api::google_shared::{GoogleFailure, retry_google_request};
use kiana_ai::types::StreamOptions;

/// 形如 `@google/genai` 的 `ApiError`：有 `status`，没有 `headers`。
fn google_api_error(status: u16) -> GoogleFailure {
    GoogleFailure {
        message: format!("got status: {status}"),
        status: Some(status),
    }
}

#[tokio::test(start_paused = true)]
async fn retries_a_headers_less_sdk_error_with_a_retryable_status() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_inner = Arc::clone(&calls);
    let result = retry_google_request(
        move || {
            let calls = Arc::clone(&calls_inner);
            async move {
                let call = calls.fetch_add(1, Ordering::SeqCst);
                if call == 0 {
                    Err(google_api_error(429))
                } else {
                    Ok("ok")
                }
            }
        },
        &StreamOptions {
            max_retries: Some(1),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(result.expect("ok"), "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn does_not_retry_when_max_retries_is_unset() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_inner = Arc::clone(&calls);
    let error = retry_google_request(
        move || {
            let calls = Arc::clone(&calls_inner);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(google_api_error(429))
            }
        },
        &StreamOptions::default(),
    )
    .await
    .expect_err("error");
    assert_eq!(error.status, Some(429));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn aborting_during_backoff_ends_the_turn_immediately() {
    // TS 的 `abortableSleep`：退避期间 abort 立刻以 AbortError("Request aborted") 结束，
    // 不等 500ms 的首次退避睡完。这里用一个外部任务在 backoff 期间取消。
    let signal = tokio_util::sync::CancellationToken::new();
    let signal_inner = signal.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_inner = Arc::clone(&calls);
    let handle = tokio::spawn(async move {
        retry_google_request(
            move || {
                let calls = Arc::clone(&calls_inner);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err::<(), _>(google_api_error(500))
                }
            },
            &StreamOptions {
                max_retries: Some(3),
                signal: Some(signal_inner),
                ..Default::default()
            },
        )
        .await
    });
    tokio::task::yield_now().await;
    signal.cancel();
    let error = handle.await.expect("join").expect_err("error");
    assert_eq!(error.message, "Request aborted");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn does_not_retry_a_non_retryable_status() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_inner = Arc::clone(&calls);
    let error = retry_google_request(
        move || {
            let calls = Arc::clone(&calls_inner);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(google_api_error(400))
            }
        },
        &StreamOptions {
            max_retries: Some(2),
            ..Default::default()
        },
    )
    .await
    .expect_err("error");
    assert_eq!(error.status, Some(400));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let _ = Duration::from_millis(0);
}

#[tokio::test(start_paused = true)]
async fn does_not_retry_transport_errors_without_a_status() {
    // TS: `isProviderError` 要求 `"status" in error`；fetch 的 TypeError 不满足，直接返回。
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_inner = Arc::clone(&calls);
    let error = retry_google_request(
        move || {
            let calls = Arc::clone(&calls_inner);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(GoogleFailure::new(
                    "fetch failed: getaddrinfo ENOTFOUND example.invalid",
                ))
            }
        },
        &StreamOptions {
            max_retries: Some(3),
            ..Default::default()
        },
    )
    .await
    .expect_err("error");
    assert_eq!(error.status, None);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
