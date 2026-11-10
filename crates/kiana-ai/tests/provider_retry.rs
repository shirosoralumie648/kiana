//! `packages/ai/test/provider-retry.test.ts`。
//!
//! TS 用 `vi.useFakeTimers()` 推进假时钟；Rust 侧用**很小的真实延迟**（`retry-after-ms: 10`、
//! `retry-after: 0.01`）来验证同一条时间线：延迟未到之前不重发，到点后才重发。这避免了
//! tokio 的 test-util 依赖，同时保持「重试发生在退避之后」这一可观察语义。
//!
//! `providerError(status, headers)` → [`ProviderError`]，响应头用 [`HeaderLookup`] 闭包提供。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use kiana_ai::utils::abort::AbortScope;
use kiana_ai::utils::provider_retry::{
    ProviderError, ProviderRetryOptions, retry_provider_request,
};

fn provider_error(
    status: Option<u16>,
    headers: Vec<(&'static str, &'static str)>,
) -> ProviderError {
    let lookup = Some(Arc::new(move |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| (*value).to_string())
    }) as Arc<dyn Fn(&str) -> Option<String> + Send + Sync>);
    ProviderError {
        message: format!(
            "Provider error: {}",
            status.map_or_else(|| "undefined".to_string(), |s| s.to_string())
        ),
        status,
        header: lookup,
    }
}

fn options(max_retries: u64) -> ProviderRetryOptions {
    ProviderRetryOptions {
        max_retries: Some(max_retries),
        ..Default::default()
    }
}

// describe("provider request retries")

#[tokio::test]
async fn retries_retryable_provider_errors() {
    let calls = Arc::new(AtomicUsize::new(0));
    let request = {
        let calls = Arc::clone(&calls);
        move || {
            let calls = Arc::clone(&calls);
            async move {
                let index = calls.fetch_add(1, Ordering::SeqCst);
                if index == 0 {
                    Err(provider_error(Some(429), vec![("retry-after-ms", "100")]))
                } else {
                    Ok("ok")
                }
            }
        }
    };
    let started = Instant::now();
    let result = retry_provider_request(request, options(1))
        .await
        .expect("ok");
    assert_eq!(result, "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    // 退避确实发生过（不是立即重试）。
    assert!(
        started.elapsed() >= Duration::from_millis(90),
        "elapsed: {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn does_not_retry_errors_the_provider_marks_as_non_retryable() {
    let calls = Arc::new(AtomicUsize::new(0));
    let request = {
        let calls = Arc::clone(&calls);
        move || {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<&str, ProviderError>(provider_error(
                    Some(429),
                    vec![("x-should-retry", "false")],
                ))
            }
        }
    };
    let error = retry_provider_request(request, options(2))
        .await
        .expect_err("non-retryable");
    assert_eq!(error.message, "Provider error: 429");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn does_not_retry_statuses_listed_in_noretrystatuses() {
    let calls = Arc::new(AtomicUsize::new(0));
    let request = {
        let calls = Arc::clone(&calls);
        move || {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<&str, ProviderError>(provider_error(Some(504), vec![("retry-after-ms", "0")]))
            }
        }
    };
    let mut opts = options(2);
    opts.no_retry_statuses = vec![504];
    let error = retry_provider_request(request, opts)
        .await
        .expect_err("no retry");
    assert_eq!(error.message, "Provider error: 504");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn rejects_a_provider_requested_retry_delay_above_the_limit() {
    let calls = Arc::new(AtomicUsize::new(0));
    let request = {
        let calls = Arc::clone(&calls);
        move || {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<&str, ProviderError>(provider_error(
                    Some(429),
                    vec![("retry-after", "277403")],
                ))
            }
        }
    };
    let mut opts = options(1);
    opts.max_retry_delay_ms = Some(1_000);
    let error = retry_provider_request(request, opts)
        .await
        .expect_err("delay cap");
    assert!(
        error
            .message
            .contains("Server requested 277403s retry delay (max: 1s)"),
        "unexpected message: {}",
        error.message
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn allows_disabling_the_provider_requested_retry_delay_cap() {
    let calls = Arc::new(AtomicUsize::new(0));
    let request = {
        let calls = Arc::clone(&calls);
        move || {
            let calls = Arc::clone(&calls);
            async move {
                let index = calls.fetch_add(1, Ordering::SeqCst);
                if index == 0 {
                    // TS 用 2 秒 + 假时钟；这里用 50ms 表达同一分支（上限关闭后按服务端值等待）。
                    Err(provider_error(Some(429), vec![("retry-after", "0.05")]))
                } else {
                    Ok("ok")
                }
            }
        }
    };
    let mut opts = options(1);
    opts.max_retry_delay_ms = Some(0);
    let started = Instant::now();
    let result = retry_provider_request(request, opts).await.expect("ok");
    assert_eq!(result, "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(
        started.elapsed() >= Duration::from_millis(40),
        "elapsed: {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn aborts_a_provider_requested_retry_delay() {
    let scope = AbortScope::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let request = {
        let calls = Arc::clone(&calls);
        move || {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<&str, ProviderError>(provider_error(
                    Some(429),
                    vec![("retry-after", "277403")],
                ))
            }
        }
    };
    let mut opts = options(2);
    opts.max_retry_delay_ms = Some(0);
    opts.signal = Some(scope.clone());
    let aborter = {
        let scope = scope.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            scope.abort();
        })
    };
    let error = retry_provider_request(request, opts)
        .await
        .expect_err("aborted");
    aborter.await.expect("aborter task");
    assert_eq!(error.name, "AbortError");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
