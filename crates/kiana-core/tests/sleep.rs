//! `utils/sleep.ts` 的测试（TS 无对应测试文件；覆盖全部公开 API）。

use std::time::Duration;

use kiana_core::sleep::sleep;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn sleep_without_signal_completes() {
    let start = tokio::time::Instant::now();
    sleep(Duration::from_millis(10), None)
        .await
        .expect("no signal");
    assert!(start.elapsed() >= Duration::from_millis(10));
}

#[tokio::test]
async fn sleep_rejects_when_the_signal_is_already_aborted() {
    let token = CancellationToken::new();
    token.cancel();
    let error = sleep(Duration::from_secs(30), Some(&token))
        .await
        .expect_err("pre-aborted signal must reject");
    assert_eq!(error.to_string(), "Aborted");
    assert_eq!(error.message(), "Aborted");
    // TS 是 `new Error("Aborted")`（name 为 `Error`）。
    assert_eq!(error.name(), "Error");
}

#[tokio::test]
async fn sleep_is_interrupted_by_abort() {
    let token = CancellationToken::new();
    let cancel = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancel.cancel();
    });

    let start = tokio::time::Instant::now();
    let error = sleep(Duration::from_secs(30), Some(&token))
        .await
        .expect_err("abort must interrupt sleep");
    assert_eq!(error.message(), "Aborted");
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn sleep_zero_duration_is_ok() {
    sleep(Duration::ZERO, None).await.expect("zero sleep");
    let token = CancellationToken::new();
    sleep(Duration::ZERO, Some(&token))
        .await
        .expect("zero sleep with signal");
}
