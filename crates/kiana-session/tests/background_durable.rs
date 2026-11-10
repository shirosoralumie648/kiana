//! durable journal 的崩溃恢复集成：新进程（新 journal 实例）能读到运行中记录并
//! terminalize + 恰好一次通知。

use std::sync::Arc;

use kiana_session::background::journal::{
    BackgroundTaskJournal, BackgroundTaskRecord, BackgroundTaskStatus,
};
use kiana_session::background::{BackgroundTaskManager, BackgroundTaskOptions, DurableTaskJournal};

fn running_record(task_id: &str, session_id: &str, output_path: &str) -> BackgroundTaskRecord {
    BackgroundTaskRecord {
        version: 1,
        task_id: task_id.to_string(),
        parent_session_id: session_id.to_string(),
        parent_turn_id: "turn-1".to_string(),
        tool: "bash".to_string(),
        description: None,
        status: BackgroundTaskStatus::Running,
        terminal_seq: 0,
        terminal: None,
        output_path: output_path.to_string(),
        output_bytes: 0,
        output_truncated: false,
        notified: false,
        attempts: 1,
        created_at_ms: 1,
        updated_at_ms: 1,
        pid: Some(std::process::id()),
        work_backed: false,
        work_id: None,
        run_id: None,
    }
}

#[tokio::test]
async fn crash_leaves_a_running_record_that_the_next_process_terminalizes_once() {
    let temp = tempfile::tempdir().expect("tempdir");
    let journal_dir = temp.path().join("journal");
    let output_dir = temp.path().join("outputs");
    std::fs::create_dir_all(&output_dir).expect("outputs");
    let output_path = output_dir.join("task_crash.output");
    std::fs::write(&output_path, "partial").expect("output");

    // 进程 A：写入运行中记录后“崩溃”（drop journal/manager，不 finish）。
    {
        let journal = Arc::new(DurableTaskJournal::new(&journal_dir));
        journal
            .save(running_record(
                "task_crash",
                "session-a",
                &output_path.to_string_lossy(),
            ))
            .await
            .expect("save");
        drop(journal);
    }

    // 进程 B：新的 journal 实例从 durable 存储读到记录，sweep 将其 terminalize 并投递
    // 一次通知。
    let journal = Arc::new(DurableTaskJournal::new(&journal_dir));
    let recoverable = journal.list_recoverable().await.expect("list");
    assert_eq!(recoverable.len(), 1, "{recoverable:?}");
    let manager = BackgroundTaskManager::new(BackgroundTaskOptions::new(
        &output_dir,
        Arc::clone(&journal) as Arc<dyn BackgroundTaskJournal>,
        Arc::clone(&journal) as Arc<dyn kiana_session::background::BackgroundTaskNotifier>,
    ));
    let report = manager.sweep().await.expect("sweep");
    assert_eq!(report.terminalized, vec!["task_crash".to_string()]);
    let notifications = journal.take_notifications();
    assert_eq!(notifications.len(), 1, "one terminal notification");
    assert_eq!(notifications[0].task_id, "task_crash");
    assert_eq!(notifications[0].status, BackgroundTaskStatus::Interrupted);
    // 生产路径（continuity）取走通知后会确认消费；否则下一次 sweep 会按
    // at-least-once 语义重放这份投递。
    journal
        .mark_deliveries_consumed(&notifications)
        .await
        .expect("mark consumed");

    // 第二次 sweep 不会再通知同一 key。
    let report = manager.sweep().await.expect("second sweep");
    assert!(report.terminalized.is_empty(), "{report:?}");
    assert!(journal.take_notifications().is_empty());

    // durable 记录保留终态。
    let record = journal
        .load("task_crash")
        .await
        .expect("load")
        .expect("record");
    assert!(record.is_terminal());
    assert_eq!(record.terminal_seq, 1);
    assert!(record.notified);
}
