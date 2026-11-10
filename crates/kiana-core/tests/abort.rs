//! `utils/abort.ts` 的测试（TS 无对应测试文件；覆盖全部公开 API）。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use kiana_core::abort::{AbortError, operation_signal, race_with_abort_signal};
use tokio_util::sync::CancellationToken;

#[test]
fn operation_signal_returns_the_given_token_or_a_fresh_one() {
    let token = CancellationToken::new();
    let returned = operation_signal(Some(token.clone()));
    assert!(!returned.is_cancelled());
    token.cancel();
    assert!(returned.is_cancelled());

    let fresh = operation_signal(None);
    assert!(!fresh.is_cancelled());
    let other = operation_signal(None);
    other.cancel();
    assert!(!fresh.is_cancelled());
}

#[test]
fn abort_error_displays_the_ts_message_and_name() {
    let error = AbortError::new();
    assert_eq!(error.to_string(), "The operation was aborted");
    assert_eq!(error.message(), "The operation was aborted");
    assert_eq!(error.name(), "AbortError");

    let as_error: &dyn std::error::Error = &error;
    assert_eq!(as_error.to_string(), "The operation was aborted");
}

#[tokio::test]
async fn race_without_signal_returns_the_operation_result() {
    let result = race_with_abort_signal(async { 42 }, None).await;
    assert_eq!(result, Ok(42));
}

#[tokio::test]
async fn race_with_cancelled_signal_drops_the_operation_and_fails_immediately() {
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    let token = CancellationToken::new();
    token.cancel();

    let result = race_with_abort_signal(
        async move {
            flag.store(true, Ordering::SeqCst);
        },
        Some(&token),
    )
    .await;

    let error = result.expect_err("cancelled signal must abort");
    assert_eq!(error.to_string(), "The operation was aborted");
    assert!(
        !ran.load(Ordering::SeqCst),
        "the dropped future must not be polled"
    );
}

#[tokio::test]
async fn race_aborts_but_keeps_driving_the_operation_to_completion() {
    let finished = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&finished);
    let token = CancellationToken::new();
    let cancel = token.clone();

    let handle = tokio::spawn(async move {
        race_with_abort_signal(
            async move {
                tokio::time::sleep(Duration::from_millis(80)).await;
                flag.store(true, Ordering::SeqCst);
                7
            },
            Some(&token),
        )
        .await
    });

    tokio::time::sleep(Duration::from_millis(10)).await;
    cancel.cancel();
    // abort 在前；operation 仍在后台任务里继续运行（TS 的 `void operation.catch(...)` 语义）。
    let error = handle
        .await
        .expect("task joins")
        .expect_err("signal cancelled before completion");
    assert_eq!(error.name(), "AbortError");

    // 立刻检查：operation 还未完成（我们只放弃等待，不取消它）。
    assert!(!finished.load(Ordering::SeqCst));

    // 等待后台任务跑完（最多 2s）。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while !finished.load(Ordering::SeqCst) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "operation was cancelled, not drained"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn race_returns_ok_when_the_operation_wins() {
    let token = CancellationToken::new();
    let result = race_with_abort_signal(
        async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            "done"
        },
        Some(&token),
    )
    .await;
    assert_eq!(result, Ok("done"));
}

#[tokio::test]
async fn abort_after_start_returns_abort_error() {
    let token = CancellationToken::new();
    let cancel = token.clone();
    let handle = tokio::spawn(async move {
        race_with_abort_signal(
            async {
                tokio::time::sleep(Duration::from_secs(30)).await;
            },
            Some(&token),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    cancel.cancel();
    let result = handle.await.expect("task joins");
    assert!(result.is_err());
}

#[tokio::test]
async fn a_panicking_operation_propagates_the_panic() {
    let handle = tokio::spawn(async {
        race_with_abort_signal(
            async {
                panic!("operation exploded");
            },
            None,
        )
        .await
    });
    let error = handle.await.expect_err("panic surfaces as JoinError");
    assert!(error.is_panic());

    let token = CancellationToken::new();
    let handle = tokio::spawn(async move {
        race_with_abort_signal(
            async {
                panic!("operation exploded");
            },
            Some(&token),
        )
        .await
    });
    let error = handle.await.expect_err("panic surfaces as JoinError");
    assert!(error.is_panic());
}
