//! 乐观用户消息队列测试：trimmed 内容 + 服务端时间戳围栏、确认 id 去重、
//! 150s watchdog 与可重试状态。

use kiana_interactive::render_core::pending_user::{
    PENDING_MESSAGE_TIMEOUT_MS, PendingStatus, PendingUserQueue,
};

#[test]
fn trimmed_content_matches_server_echo() {
    let mut queue = PendingUserQueue::new();
    queue.enqueue("c1", "  hello world  ", None, Vec::new(), Vec::new(), 0);
    let consumed = queue
        .consume_matching("c1", "hello world", "event-1", 10)
        .expect("trimmed match");
    assert_eq!(consumed.message.text, "  hello world  ");
    assert!(queue.messages().is_empty());
}

#[test]
fn server_timestamp_fence_rejects_stale_echo() {
    let mut queue = PendingUserQueue::new();
    queue.observe_server_timestamp("c1", Some(1_000));
    queue.enqueue("c1", "hello", None, Vec::new(), Vec::new(), 0);
    assert!(
        queue
            .consume_matching("c1", "hello", "event-1", 1_000)
            .is_none(),
        "equal timestamp must not satisfy the strict fence"
    );
    assert!(
        queue
            .consume_matching("c1", "hello", "event-1", 1_001)
            .is_some()
    );
}

#[test]
fn confirmation_id_dedupe_prevents_double_consume_on_replay() {
    let mut queue = PendingUserQueue::new();
    queue.enqueue("c1", "same", None, Vec::new(), Vec::new(), 0);
    queue.enqueue("c1", "same", None, Vec::new(), Vec::new(), 0);
    assert!(
        queue
            .consume_matching("c1", "same", "event-1", 10)
            .is_some()
    );
    assert!(
        queue
            .consume_matching("c1", "same", "event-1", 10)
            .is_none(),
        "replayed confirmation must be ignored"
    );
    assert!(
        queue
            .consume_matching("c1", "same", "event-2", 11)
            .is_some(),
        "a different event still consumes the second pending message"
    );
    assert_eq!(queue.confirmed_count(), 2);
}

#[test]
fn watchdog_turns_sending_into_retryable_error_after_150s() {
    let mut queue = PendingUserQueue::new();
    let id = queue.enqueue("c1", "hi", None, Vec::new(), Vec::new(), 0);
    assert_eq!(queue.tick(PENDING_MESSAGE_TIMEOUT_MS - 1), 0);
    assert_eq!(queue.tick(PENDING_MESSAGE_TIMEOUT_MS), 1);
    assert_eq!(
        queue.messages()[0].status,
        PendingStatus::Error {
            message: "Send timed out".to_string()
        }
    );
    assert!(queue.messages()[0].retryable());

    assert!(queue.mark_sending(&id, PENDING_MESSAGE_TIMEOUT_MS));
    assert_eq!(queue.messages()[0].status, PendingStatus::Sending);
    assert_eq!(
        queue.tick(PENDING_MESSAGE_TIMEOUT_MS + PENDING_MESSAGE_TIMEOUT_MS - 1),
        0
    );
    assert_eq!(
        queue.tick(PENDING_MESSAGE_TIMEOUT_MS + PENDING_MESSAGE_TIMEOUT_MS),
        1
    );
}

#[test]
fn late_echo_can_still_consume_an_errored_message() {
    let mut queue = PendingUserQueue::new();
    queue.enqueue("c1", "hi", None, Vec::new(), Vec::new(), 0);
    queue.tick(PENDING_MESSAGE_TIMEOUT_MS);
    let consumed = queue
        .consume_matching("c1", "hi", "event", 10)
        .expect("error entry is still consumable");
    assert!(consumed.message.retryable());
}

#[test]
fn conversation_isolation_and_reassignment() {
    let mut queue = PendingUserQueue::new();
    queue.enqueue("c1", "one", None, Vec::new(), Vec::new(), 0);
    queue.enqueue("c2", "two", None, Vec::new(), Vec::new(), 0);
    assert!(queue.consume_matching("c2", "one", "e1", 1).is_none());
    assert_eq!(queue.messages_for("c1").len(), 1);
    assert_eq!(queue.reassign_conversation("c1", "task-1"), 1);
    assert_eq!(queue.messages_for("task-1").len(), 1);
}

#[test]
fn retry_refreshes_fence_and_watchdog_clock() {
    let mut queue = PendingUserQueue::new();
    let id = queue.enqueue("c1", "hello", None, Vec::new(), Vec::new(), 0);
    queue.observe_server_timestamp("c1", Some(500));
    queue.mark_error(&id, "network");
    assert!(queue.mark_sending(&id, 1_000));
    // 围栏在重试时刷新为 500：500 的回显不接受，501 接受。
    assert!(queue.consume_matching("c1", "hello", "old", 500).is_none());
    assert!(queue.consume_matching("c1", "hello", "new", 501).is_some());
}

#[test]
fn fences_are_isolated_per_conversation() {
    let mut queue = PendingUserQueue::new();
    queue.observe_server_timestamp("c1", Some(1_000));
    queue.observe_server_timestamp("c2", Some(2_000));
    queue.enqueue("c1", "one", None, Vec::new(), Vec::new(), 0);
    queue.enqueue("c2", "two", None, Vec::new(), Vec::new(), 0);
    assert_eq!(queue.fence_for("c1"), Some(1_000));
    assert_eq!(queue.fence_for("c2"), Some(2_000));
    assert!(queue.consume_matching("c2", "two", "e2", 1_500).is_none());
    assert!(queue.consume_matching("c1", "one", "e1", 1_001).is_some());
    assert!(queue.consume_matching("c2", "two", "e2", 2_001).is_some());
}
