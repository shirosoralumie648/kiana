//! `test/session-adoption.test.ts` 的端口（节选）。

mod support;

use kiana_durable::Session;
use kiana_durable::SessionEnd;
use kiana_durable::session::SessionImpl;
use serde_json::json;
use support::*;

fn session_target(token: &kiana_durable::DocToken) -> kiana_durable::DocTarget {
    kiana_durable::DocTarget::session(token)
}

async fn bump_session_doc(
    session: &SessionImpl,
    token: &kiana_durable::DocToken,
) -> Result<(), kiana_durable::BoxError> {
    let token = token.clone();
    commit(session, move |tx| {
        let target = session_target(&token);
        Box::pin(async move {
            let mut draft = tx.doc(&target).await?;
            let count = draft
                .get("count")
                .and_then(kiana_durable::JsonValue::as_i64)
                .unwrap_or(0);
            draft.insert("count".to_owned(), json!(count + 1));
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn adopts_synchronously_inside_the_commit_before_storage_resolves() {
    let (storage, session, _publications) = open_test_session();
    let token = session_definition("adoption.counter", 1);
    support::lock(&storage.order).clear();

    bump_session_doc(&session, &token)
        .await
        .expect("commit succeeds");

    assert_eq!(
        *support::lock(&storage.order),
        vec!["adopt", "adopted", "resolve"]
    );
    let snapshot = session
        .snapshot(
            &session_target(&token),
            &**kiana_chord::context::BACKGROUND_CONTEXT,
        )
        .await
        .unwrap()
        .expect("adopted document is cached");
    assert_eq!(snapshot.get("count"), Some(&json!(1)));
    session
        .close(&**kiana_chord::context::BACKGROUND_CONTEXT)
        .await;
}

#[tokio::test]
async fn fails_the_session_without_discarding_when_storage_rejects_after_adoption() {
    let (storage, session, _publications) = open_test_session();
    let token = session_definition("adoption.counter", 1);
    bump_session_doc(&session, &token)
        .await
        .expect("commit succeeds");

    *support::lock(&storage.fault) = Some(CommitFault::RejectAfterAdopt);
    let error = bump_session_doc(&session, &token).await.unwrap_err();
    assert_eq!(error.to_string(), "reclaim failed");
    match session.closed().await {
        SessionEnd::Failed { error } => assert_eq!(error.to_string(), "reclaim failed"),
        SessionEnd::Closed => panic!("a rejected commit must fail the Session"),
    }
}

#[tokio::test]
async fn fails_the_session_when_storage_resolves_without_adopting() {
    let (storage, session, _publications) = open_test_session();
    let token = session_definition("adoption.counter", 1);
    bump_session_doc(&session, &token)
        .await
        .expect("commit succeeds");

    *support::lock(&storage.fault) = Some(CommitFault::SkipAdopt);
    let error = bump_session_doc(&session, &token).await.unwrap_err();
    assert!(
        error.to_string().contains("without adopting"),
        "unexpected error: {error}"
    );
    match session.closed().await {
        SessionEnd::Failed { .. } => {}
        SessionEnd::Closed => panic!("a missing adoption must fail the Session"),
    }
}

#[tokio::test]
async fn discards_the_staged_changes_when_storage_rejects_before_adoption() {
    let (storage, session, _publications) = open_test_session();
    let token = session_definition("adoption.counter.discard", 1);
    bump_session_doc(&session, &token)
        .await
        .expect("commit succeeds");

    storage.fail_next_commit(std::io::Error::other("disk full").into());
    let error = bump_session_doc(&session, &token).await.unwrap_err();
    assert_eq!(error.to_string(), "disk full");
    match session.closed().await {
        SessionEnd::Failed { error } => assert_eq!(error.to_string(), "disk full"),
        SessionEnd::Closed => panic!("a rejected commit must fail the Session"),
    }
    // 没有任何变化到达 Storage：重新打开后仍是上一次成功提交的值。
    storage.reopen();
    let reopened = SessionImpl::new(
        storage,
        kiana_durable::session::SessionOptions {
            document_retention_ms: Some(600_000.0),
            ..Default::default()
        },
    );
    let snapshot = reopened
        .snapshot(
            &session_target(&token),
            &**kiana_chord::context::BACKGROUND_CONTEXT,
        )
        .await
        .unwrap()
        .expect("the document exists");
    assert_eq!(snapshot.get("count"), Some(&json!(1)));
    reopened
        .close(&**kiana_chord::context::BACKGROUND_CONTEXT)
        .await;
}

// ---------------------------------------------------------------------------
// 回归：GuardedStorage 的在途计数与关闭上下文
// ---------------------------------------------------------------------------

#[tokio::test]
async fn close_completes_after_an_in_flight_storage_call_is_dropped() {
    let (storage, session, _publications) = open_test_session();
    let token = session_definition("adoption.counter", 1);
    let target = session_target(&token);
    let gate = storage.hold_find_document();
    {
        let mut pending =
            Box::pin(session.snapshot(&target, &**kiana_chord::context::BACKGROUND_CONTEXT));
        let timed = tokio::time::timeout(std::time::Duration::from_millis(200), &mut pending).await;
        assert!(timed.is_err(), "the read is held at the storage gate");
        // 调用方在受保护的后端调用进行中离开（timeout/select!/被取消的 task）。
        drop(pending);
    }
    gate.release();
    let closed = tokio::time::timeout(
        std::time::Duration::from_millis(1000),
        session.close(&**kiana_chord::context::BACKGROUND_CONTEXT),
    )
    .await;
    assert!(closed.is_ok(), "close must not hang after a cancelled call");
}

#[tokio::test]
async fn close_passes_the_callers_context_without_its_abort_signal_to_storage() {
    use kiana_chord::context::{
        AbortSignal, BACKGROUND_CONTEXT, ContextRef, with_abort_signal, with_context_value,
    };
    use std::sync::Arc;

    let (storage, session, _publications) = open_test_session();
    let signal = AbortSignal::new();
    let with_value: ContextRef = with_context_value(
        support::CLOSE_PROBE_KEY.clone(),
        "cleanup".to_owned(),
        Arc::clone(&BACKGROUND_CONTEXT),
    );
    let caller = with_abort_signal(signal, with_value);
    session.close(Arc::clone(&caller)).await;

    assert_eq!(
        *support::lock(&storage.close_probe),
        Some(("cleanup".to_owned(), false)),
        "storage.close() sees the caller's values and no abort signal"
    );
}

// ---------------------------------------------------------------------------
// 回归：trait 的 `subscribe_commits`/`subscribe_close` 以抛出的错误对象 panic
// （TS 的 `assertUsable()`：失败时 `SessionFailed`，关闭后 `Error("Session is closed")`）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn trait_subscriptions_panic_with_the_thrown_error_object() {
    use std::sync::Arc;

    let (_storage, session, _publications) = open_test_session();
    session
        .close(&**kiana_chord::context::BACKGROUND_CONTEXT)
        .await;

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Session::subscribe_commits(&session, Arc::new(|_, _| {}))
    }))
    .err()
    .expect("a closed Session rejects commit listeners");
    let error = panic
        .downcast::<kiana_durable::BoxError>()
        .expect("the panic payload is the thrown error object");
    assert!(
        error
            .downcast_ref::<kiana_durable::session::SessionClosed>()
            .is_some(),
        "unexpected error: {error}"
    );
    assert_eq!(error.to_string(), "Session is closed");

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Session::subscribe_close(&session, Arc::new(|| {}))
    }))
    .err()
    .expect("a closed Session rejects close listeners");
    let error = panic
        .downcast::<kiana_durable::BoxError>()
        .expect("the panic payload is the thrown error object");
    assert!(
        error
            .downcast_ref::<kiana_durable::session::SessionClosed>()
            .is_some(),
        "unexpected error: {error}"
    );

    // 失败后：TS 的 `assertUsable()` 抛 `SessionFailed(cause)`。
    let (storage, session, _publications) = open_test_session();
    let token = session_definition("adoption.subscribe", 1);
    bump_session_doc(&session, &token)
        .await
        .expect("the first commit succeeds");
    storage.fail_next_commit(std::io::Error::other("disk gone").into());
    let _ = bump_session_doc(&session, &token)
        .await
        .expect_err("the injected failure rejects the commit");
    assert!(matches!(session.closed().await, SessionEnd::Failed { .. }));

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Session::subscribe_commits(&session, Arc::new(|_, _| {}))
    }))
    .err()
    .expect("a failed Session rejects commit listeners");
    let error = panic
        .downcast::<kiana_durable::BoxError>()
        .expect("the panic payload is the thrown error object");
    let failed = error
        .downcast_ref::<kiana_durable::SessionFailed>()
        .expect("a failed Session throws SessionFailed");
    assert_eq!(failed.cause.to_string(), "disk gone");
}
