//! 后台任务（`background/`）测试。
//!
//! 覆盖 DoD：
//! - `background=true` 立即返回 task id；父 turn 结束不杀后台任务；
//! - 输出文件硬上限 + O_NOFOLLOW；
//! - terminal 通知按 task id + terminal seq 恰好一次；
//! - cancel 传播并抑制迟到通知；
//! - token usage 归因到父会话和 task；
//! - 崩溃后 sweep 恢复或 terminalize，重发未送达的终态通知。

mod common;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kiana_session::background::output::BackgroundTaskOutput;
use kiana_session::background::{
    BackgroundTaskJournal, BackgroundTaskManager, BackgroundTaskOptions, BackgroundTaskRecord,
    BackgroundTaskResumer, BackgroundTaskSpec, BackgroundTaskStatus, BackgroundTaskTerminal,
    MemoryTaskJournal, RecordingNotifier, TaskOutcome, TaskUsage,
};
use tokio::sync::oneshot;

fn manager(
    dir: &std::path::Path,
    journal: Arc<MemoryTaskJournal>,
    notifier: Arc<RecordingNotifier>,
    max_output_bytes: u64,
) -> Arc<BackgroundTaskManager> {
    BackgroundTaskManager::new(
        BackgroundTaskOptions::new(dir.to_path_buf(), journal, notifier)
            .with_max_output_bytes(max_output_bytes),
    )
}

async fn wait_for_notifications(notifier: &RecordingNotifier, expected: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if notifier.len() >= expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("timed out waiting for notification");
}

#[tokio::test]
async fn start_returns_immediately_and_task_survives_parent_turn() {
    let temp = tempfile::tempdir().expect("temp");
    let journal = Arc::new(MemoryTaskJournal::new());
    let notifier = Arc::new(RecordingNotifier::new());
    let manager = manager(
        &temp.path().join("outputs"),
        journal.clone(),
        notifier.clone(),
        1024,
    );

    let handle = manager
        .start(
            BackgroundTaskSpec::new("parent-session", "turn-1", "bash")
                .with_description("long build"),
            |context| async move {
                context.output.append("working...\n").expect("append");
                tokio::time::sleep(Duration::from_millis(40)).await;
                TaskOutcome::completed("finished\n").with_usage(TaskUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                    total_tokens: 15,
                })
            },
        )
        .await
        .expect("start");
    assert!(handle.task_id.starts_with("task_"));
    assert!(handle.output_path.exists());

    // 父 turn 结束：这里直接 drop 一个代表父 turn 的 future，任务不受影响。
    let parent_turn = async {
        tokio::time::sleep(Duration::from_millis(1)).await;
    };
    parent_turn.await;

    wait_for_notifications(&notifier, 1).await;
    let notifications = notifier.notifications();
    assert_eq!(notifications.len(), 1);
    let notification = &notifications[0];
    assert_eq!(notification.task_id, handle.task_id);
    assert_eq!(notification.terminal_seq, 1);
    assert_eq!(notification.status, BackgroundTaskStatus::Completed);
    assert_eq!(notification.parent_session_id, "parent-session");
    assert_eq!(notification.parent_turn_id, "turn-1");
    assert_eq!(notification.usage.input_tokens, 10);

    let record = manager
        .task(&handle.task_id)
        .await
        .expect("task")
        .expect("record");
    assert!(record.is_terminal());
    assert!(record.notified);
    assert_eq!(record.terminal_seq, 1);

    let output = fs::read_to_string(&handle.output_path).expect("output");
    assert!(output.contains("working..."));
    assert!(output.contains("finished"));

    // usage 同时归因到父会话和 task。
    let parent = manager.usage_for_parent("parent-session");
    let task = manager.usage_for_task(&handle.task_id);
    assert_eq!(parent, task);
    assert_eq!(parent.total_tokens, 15);
}

/// 回归（Phase 1c）：`start_local` 必须在调用方的 LocalSet 上运行 `!Send` future。
/// `Rc<Cell>` 捕获会让 future 无法通过 `Send` 编译检查，证明它确实走 `spawn_local`。
#[tokio::test]
async fn start_local_runs_a_non_send_future_on_the_local_set() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let temp = tempfile::tempdir().expect("temp");
            let journal = Arc::new(MemoryTaskJournal::new());
            let notifier = Arc::new(RecordingNotifier::new());
            let manager = manager(
                &temp.path().join("outputs"),
                journal.clone(),
                notifier.clone(),
                1024,
            );
            let marker = std::rc::Rc::new(std::cell::Cell::new(0u32));
            let marker_in_task = std::rc::Rc::clone(&marker);
            let handle = manager
                .start_local(
                    BackgroundTaskSpec::new("local-session", "work:1", "agent"),
                    move |context| async move {
                        marker_in_task.set(marker_in_task.get() + 1);
                        context.output.append("local stage\n").expect("append");
                        TaskOutcome::completed("local done\n")
                    },
                )
                .await
                .expect("start_local");
            wait_for_notifications(&notifier, 1).await;
            assert_eq!(marker.get(), 1, "the !Send future must run once");
            let record = manager
                .task(&handle.task_id)
                .await
                .expect("task")
                .expect("record");
            assert!(record.is_terminal());
            assert_eq!(record.status, BackgroundTaskStatus::Completed);
            let output = fs::read_to_string(&handle.output_path).expect("output");
            assert!(output.contains("local stage"));
            assert!(output.contains("local done"));
        })
        .await;
}

/// 回归（Phase 1c）：`start_local` 的取消是协作式的——`cancel` 抢占 journal 终态，
/// 但不 abort future；Work 生命周期仍要能继续运行并写出自己的终态。
#[tokio::test]
async fn cancel_of_a_local_task_is_cooperative() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let temp = tempfile::tempdir().expect("temp");
            let journal = Arc::new(MemoryTaskJournal::new());
            let notifier = Arc::new(RecordingNotifier::new());
            let manager = manager(
                &temp.path().join("outputs"),
                journal.clone(),
                notifier.clone(),
                1024,
            );
            let (started, wait_started) = oneshot::channel::<()>();
            let (observed, wait_observed) = oneshot::channel::<()>();
            let handle = manager
                .start_local(
                    BackgroundTaskSpec::new("local-session", "work:2", "agent"),
                    move |context| async move {
                        let _ = started.send(());
                        context.cancel.cancelled().await;
                        let _ = observed.send(());
                        // 取消后仍返回终态：真实 Work 生命周期在这里写 Work 账本。
                        TaskOutcome::cancelled()
                    },
                )
                .await
                .expect("start_local");
            wait_started.await.expect("task started");
            assert!(manager.cancel(&handle.task_id).await.expect("cancel"));
            tokio::time::timeout(Duration::from_secs(5), wait_observed)
                .await
                .expect("local task must not be aborted by cancel")
                .expect("task observed cancellation");
            // 协作式取消不抢写终态：任务自己的 finish_task 才把它落成 Cancelled。
            wait_for_notifications(&notifier, 1).await;
            assert_eq!(
                manager
                    .task(&handle.task_id)
                    .await
                    .expect("task")
                    .expect("record")
                    .status,
                BackgroundTaskStatus::Cancelled
            );
            // 迟到完成不能改写 journal/通知。
            let late = manager
                .finish_task(&handle.task_id, TaskOutcome::completed("late"))
                .await
                .expect("late finish");
            assert!(!late.won_race);
            assert!(late.notification.is_none());
            assert_eq!(notifier.len(), 1);
        })
        .await;
}

#[tokio::test]
async fn completion_race_has_one_winner_and_one_notification() {
    let temp = tempfile::tempdir().expect("temp");
    let journal = Arc::new(MemoryTaskJournal::new());
    let notifier = Arc::new(RecordingNotifier::new());
    let manager = manager(
        &temp.path().join("outputs"),
        journal.clone(),
        notifier.clone(),
        1024,
    );

    let handle = manager
        .start(
            BackgroundTaskSpec::new("session", "turn", "bash"),
            |_context| async move { std::future::pending::<TaskOutcome>().await },
        )
        .await
        .expect("start");

    let (first, second) = tokio::join!(
        manager.finish_task(&handle.task_id, TaskOutcome::completed("first")),
        manager.finish_task(&handle.task_id, TaskOutcome::completed("second")),
    );
    let first = first.expect("first");
    let second = second.expect("second");
    assert_ne!(first.won_race, second.won_race);
    assert_eq!(notifier.len(), 1);
    let record = manager
        .task(&handle.task_id)
        .await
        .expect("task")
        .expect("record");
    assert_eq!(record.terminal_seq, 1);
}

#[tokio::test]
async fn cancel_propagates_and_suppresses_late_completion_notification() {
    let temp = tempfile::tempdir().expect("temp");
    let journal = Arc::new(MemoryTaskJournal::new());
    let notifier = Arc::new(RecordingNotifier::new());
    let manager = manager(
        &temp.path().join("outputs"),
        journal.clone(),
        notifier.clone(),
        1024,
    );

    let (release, wait) = oneshot::channel::<()>();
    let handle = manager
        .start(
            BackgroundTaskSpec::new("session", "turn", "bash"),
            move |_context| async move {
                let _ = wait.await;
                TaskOutcome::completed("late")
            },
        )
        .await
        .expect("start");

    assert!(manager.cancel(&handle.task_id).await.expect("cancel"));
    assert_eq!(notifier.len(), 1);
    assert_eq!(
        notifier.notifications()[0].status,
        BackgroundTaskStatus::Cancelled
    );

    // 迟到完成：直接调用 finish_task，必须被抑制。
    let late = manager
        .finish_task(&handle.task_id, TaskOutcome::completed("late"))
        .await
        .expect("late finish");
    assert!(!late.won_race);
    assert!(late.notification.is_none());
    assert_eq!(notifier.len(), 1);

    // 再次 cancel 是幂等的。
    assert!(!manager.cancel(&handle.task_id).await.expect("cancel again"));
    drop(release);
}

#[tokio::test]
async fn output_file_is_hard_capped_and_truncation_is_reported() {
    let temp = tempfile::tempdir().expect("temp");
    let journal = Arc::new(MemoryTaskJournal::new());
    let notifier = Arc::new(RecordingNotifier::new());
    let manager = manager(
        &temp.path().join("outputs"),
        journal.clone(),
        notifier.clone(),
        16,
    );

    let handle = manager
        .start(
            BackgroundTaskSpec::new("session", "turn", "bash"),
            |context| async move {
                context.output.append("0123456789abcdef").expect("first");
                context
                    .output
                    .append("this must be dropped")
                    .expect("second");
                TaskOutcome::completed("0123456789abcdef-final")
            },
        )
        .await
        .expect("start");

    wait_for_notifications(&notifier, 1).await;
    let metadata = fs::metadata(&handle.output_path).expect("metadata");
    assert!(metadata.len() <= 16, "output must respect the hard cap");
    let record = manager
        .task(&handle.task_id)
        .await
        .expect("task")
        .expect("record");
    assert!(record.output_truncated);
    assert!(notifier.notifications()[0].output_truncated);
}

#[cfg(unix)]
#[test]
fn output_open_rejects_symlink() {
    let temp = tempfile::tempdir().expect("temp");
    let target = temp.path().join("target");
    fs::write(&target, "secret").expect("target");
    let link = temp.path().join("task.output");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");
    let error = BackgroundTaskOutput::open(&link, 1024).expect_err("symlink must be rejected");
    // Linux ELOOP（O_NOFOLLOW 拒绝最终路径的符号链接）。
    assert_eq!(error.raw_os_error(), Some(40), "unexpected error: {error}");
}

fn running_record(task_id: &str, output_path: PathBuf, attempts: u32) -> BackgroundTaskRecord {
    BackgroundTaskRecord {
        version: 1,
        task_id: task_id.to_string(),
        parent_session_id: "parent".to_string(),
        parent_turn_id: "turn".to_string(),
        tool: "bash".to_string(),
        description: None,
        status: BackgroundTaskStatus::Running,
        terminal_seq: 0,
        terminal: None,
        output_path: output_path.to_string_lossy().into_owned(),
        output_bytes: 0,
        output_truncated: false,
        notified: false,
        attempts,
        created_at_ms: 1,
        updated_at_ms: 1,
        // 未知 pid 无法证明记录仍被占用：sweep 按“进程丢失”处理。
        pid: None,
        work_backed: false,
        work_id: None,
        run_id: None,
    }
}

struct FixedResumer {
    output: String,
    usage: TaskUsage,
}

#[async_trait]
impl BackgroundTaskResumer for FixedResumer {
    async fn resume(
        &self,
        _record: &BackgroundTaskRecord,
    ) -> Result<Option<TaskOutcome>, kiana_session::background::BackgroundTaskError> {
        Ok(Some(
            TaskOutcome::completed(self.output.clone()).with_usage(self.usage),
        ))
    }
}

#[tokio::test]
async fn cancel_lets_the_task_run_its_cooperative_cleanup() {
    let temp = tempfile::tempdir().expect("temp");
    let journal = Arc::new(MemoryTaskJournal::new());
    let notifier = Arc::new(RecordingNotifier::new());
    let manager = manager(
        &temp.path().join("outputs"),
        journal.clone(),
        notifier.clone(),
        1024,
    );
    let (cleanup_started, cleanup_observed) = oneshot::channel::<()>();
    let handle = manager
        .start(
            BackgroundTaskSpec::new("parent", "turn", "bash"),
            move |context| async move {
                context.cancel.cancelled().await;
                let _ = cleanup_started.send(());
                // 模拟 kill_process_tree：取消后还需要一小段时间收尾。
                tokio::time::sleep(Duration::from_millis(30)).await;
                TaskOutcome::cancelled()
            },
        )
        .await
        .expect("start");
    assert!(manager.cancel(&handle.task_id).await.expect("cancel"));
    tokio::time::timeout(Duration::from_secs(2), cleanup_observed)
        .await
        .expect("cancel must not abort before the cleanup runs")
        .expect("cleanup signal");
    assert_eq!(
        manager
            .task(&handle.task_id)
            .await
            .expect("task")
            .expect("record")
            .status,
        BackgroundTaskStatus::Cancelled
    );
}

/// Running 记录的 `output_bytes` 从 prepare 后不再更新（通常是 0）；terminalize
/// 必须以文件实际长度为准，否则崩溃恢复会把已有输出报告成 0 字节。
#[tokio::test]
async fn sweep_reports_the_real_output_size_for_a_lost_task() {
    let temp = tempfile::tempdir().expect("temp");
    let journal = Arc::new(MemoryTaskJournal::new());
    let notifier = Arc::new(RecordingNotifier::new());
    let output_path = temp.path().join("lost.output");
    fs::write(&output_path, b"partial output").expect("output");
    journal
        .save(running_record("task_bytes", output_path, 1))
        .await
        .expect("save");

    let manager = manager(
        &temp.path().join("outputs"),
        journal.clone(),
        notifier.clone(),
        1024,
    );
    manager.sweep().await.expect("sweep");

    let record = journal
        .load("task_bytes")
        .await
        .expect("load")
        .expect("record");
    assert_eq!(record.status, BackgroundTaskStatus::Interrupted);
    assert_eq!(record.output_bytes, "partial output".len() as u64);
    assert_eq!(notifier.notifications()[0].output_bytes, 14);
}

#[cfg(unix)]
#[tokio::test]
async fn sweep_skips_tasks_owned_by_a_live_process() {
    let temp = tempfile::tempdir().expect("temp");
    let journal = Arc::new(MemoryTaskJournal::new());
    let notifier = Arc::new(RecordingNotifier::new());
    let output_path = temp.path().join("live.output");
    // 用真实存活的子进程充当“另一个正在运行的 kiana 进程”。
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn live process");
    let mut record = running_record("task_live", output_path, 1);
    record.pid = Some(child.id());
    journal.save(record).await.expect("save");

    let manager = manager(
        &temp.path().join("outputs"),
        journal.clone(),
        notifier.clone(),
        1024,
    );
    let report = manager.sweep().await.expect("sweep");
    assert_eq!(report.skipped_live, vec!["task_live".to_string()]);
    assert!(report.terminalized.is_empty());
    assert_eq!(notifier.len(), 0);
    assert!(
        !journal
            .load("task_live")
            .await
            .expect("load")
            .expect("record")
            .is_terminal(),
        "another live process still owns the task"
    );

    let _ = child.kill();
    let _ = child.wait();
}

/// 回归（Phase 1c 设计评审 critical）：Work 驱动的 attempt 由 Work 账本对账，通用
/// sweep 不能替它宣告终态。否则另一个会话启动时会把“Work 已成功、finish_task 尚未
/// 执行”的崩溃窗口误写成 `process_lost`，而终态 CAS 一旦落定就无法再被修正。
#[tokio::test]
async fn sweep_leaves_work_backed_attempts_to_the_work_ledger() {
    let temp = tempfile::tempdir().expect("temp");
    let journal = Arc::new(MemoryTaskJournal::new());
    let notifier = Arc::new(RecordingNotifier::new());
    let mut record = running_record("task_work", temp.path().join("work.output"), 1);
    record.tool = "agent".to_string();
    record.work_backed = true;
    journal.save(record).await.expect("save");
    let manager = manager(
        &temp.path().join("outputs"),
        journal.clone(),
        notifier.clone(),
        1024,
    );

    let report = manager.sweep().await.expect("sweep");
    assert_eq!(
        report.skipped_work_backed,
        vec!["task_work".to_string()],
        "{report:?}"
    );
    assert!(report.terminalized.is_empty(), "{report:?}");
    assert!(report.resumed.is_empty(), "{report:?}");
    let record = journal
        .load("task_work")
        .await
        .expect("load")
        .expect("record");
    assert!(
        !record.is_terminal(),
        "only the owning session's Work reconciliation may close this attempt"
    );
    assert_eq!(
        record.attempts, 1,
        "the generic sweep must not consume Work resume attempts"
    );
    assert_eq!(notifier.len(), 0);
}

/// 回归（Phase 1c 设计评审）：没有执行槽时不能代为落定终态。同进程的另一个会话
/// （另一个 manager）可能正跑着这条记录；抢写 `Cancelled` 会丢掉 Work 的权威结果。
#[tokio::test]
async fn cancel_does_not_claim_the_terminal_of_a_task_owned_by_another_manager() {
    let temp = tempfile::tempdir().expect("temp");
    let journal = Arc::new(MemoryTaskJournal::new());
    let notifier = Arc::new(RecordingNotifier::new());
    let mut record = running_record("task_other", temp.path().join("other.output"), 1);
    record.pid = Some(std::process::id());
    journal.save(record).await.expect("save");
    let manager = manager(
        &temp.path().join("a"),
        journal.clone(),
        notifier.clone(),
        1024,
    );

    assert!(
        !manager.cancel("task_other").await.expect("cancel"),
        "no execution slot means the cancel request was not delivered"
    );
    let record = journal
        .load("task_other")
        .await
        .expect("load")
        .expect("record");
    assert!(
        !record.is_terminal(),
        "another manager still owns the attempt's terminal"
    );
    assert_eq!(notifier.len(), 0);
}

/// 回归（Phase 1c 设计评审）：`finish_task` 先追加输出、后提交 CAS。崩溃发生在两者
/// 之间时，恢复重放必须先回到记录里的长度，不能把同一段终态输出写两遍。
#[tokio::test]
async fn recovery_truncates_output_appended_before_a_crashed_terminal_commit() {
    let temp = tempfile::tempdir().expect("temp");
    let journal = Arc::new(MemoryTaskJournal::new());
    let notifier = Arc::new(RecordingNotifier::new());
    let output_path = temp.path().join("task_recover.output");
    // 记录里 output_bytes=5，但文件已经包含了崩溃前追加的终态输出。
    fs::write(&output_path, "base\nstale\n").expect("output");
    let mut record = running_record("task_recover", output_path.clone(), 1);
    record.output_bytes = 5;
    journal.save(record).await.expect("save");
    let manager = manager(
        &temp.path().join("outputs"),
        journal.clone(),
        notifier.clone(),
        1024,
    );

    manager
        .finish_task("task_recover", TaskOutcome::completed("fresh\n"))
        .await
        .expect("finish_task");
    let written = fs::read_to_string(&output_path).expect("read output");
    assert_eq!(
        written, "base\nfresh\n",
        "the stale terminal append must be truncated before replay"
    );
    let record = manager
        .task("task_recover")
        .await
        .expect("task")
        .expect("record");
    assert_eq!(record.output_bytes, written.len() as u64);
    wait_for_notifications(&notifier, 1).await;
}

#[tokio::test]
async fn sweep_terminalizes_lost_tasks_and_does_not_repeat_notifications() {
    let temp = tempfile::tempdir().expect("temp");
    let journal = Arc::new(MemoryTaskJournal::new());
    let notifier = Arc::new(RecordingNotifier::new());
    let output_path = temp.path().join("lost.output");
    journal
        .save(running_record("task_lost", output_path, 1))
        .await
        .expect("save");

    let manager = manager(
        &temp.path().join("outputs"),
        journal.clone(),
        notifier.clone(),
        1024,
    );
    let report = manager.sweep().await.expect("sweep");
    assert_eq!(report.terminalized, vec!["task_lost".to_string()]);
    assert_eq!(notifier.len(), 1);
    assert_eq!(
        notifier.notifications()[0].status,
        BackgroundTaskStatus::Interrupted
    );
    assert_eq!(
        notifier.notifications()[0].reason.as_deref(),
        Some("process_lost")
    );

    // 再 sweep 不会重复 terminal/notification。
    let second = manager.sweep().await.expect("second sweep");
    assert!(second.terminalized.is_empty());
    assert_eq!(notifier.len(), 1);
}

#[tokio::test]
async fn sweep_resumes_when_a_resumer_is_available_and_attributes_usage() {
    let temp = tempfile::tempdir().expect("temp");
    let journal = Arc::new(MemoryTaskJournal::new());
    let notifier = Arc::new(RecordingNotifier::new());
    let output_path = temp.path().join("resume.output");
    journal
        .save(running_record("task_resume", output_path.clone(), 1))
        .await
        .expect("save");

    let resumer = Arc::new(FixedResumer {
        output: "resumed-output\n".to_string(),
        usage: TaskUsage {
            input_tokens: 3,
            output_tokens: 2,
            total_tokens: 5,
        },
    });
    let manager = BackgroundTaskManager::new(
        BackgroundTaskOptions::new(
            temp.path().join("outputs"),
            journal.clone(),
            notifier.clone(),
        )
        .with_resumer(resumer),
    );
    let report = manager.sweep().await.expect("sweep");
    assert_eq!(report.resumed, vec!["task_resume".to_string()]);
    assert_eq!(notifier.len(), 1);
    assert_eq!(
        notifier.notifications()[0].status,
        BackgroundTaskStatus::Completed
    );
    assert_eq!(notifier.notifications()[0].terminal_seq, 1);
    assert!(
        fs::read_to_string(&output_path)
            .expect("output")
            .contains("resumed-output")
    );
    assert_eq!(manager.usage_for_parent("parent").total_tokens, 5);
    assert_eq!(manager.usage_for_task("task_resume").total_tokens, 5);
}

#[tokio::test]
async fn sweep_terminalizes_after_resume_attempts_are_exhausted() {
    let temp = tempfile::tempdir().expect("temp");
    let journal = Arc::new(MemoryTaskJournal::new());
    let notifier = Arc::new(RecordingNotifier::new());
    journal
        .save(running_record(
            "task_exhausted",
            temp.path().join("exhausted.output"),
            3,
        ))
        .await
        .expect("save");

    let manager = BackgroundTaskManager::new(
        BackgroundTaskOptions::new(
            temp.path().join("outputs"),
            journal.clone(),
            notifier.clone(),
        )
        .with_max_resume_attempts(3)
        .with_resumer(Arc::new(FixedResumer {
            output: String::new(),
            usage: TaskUsage::default(),
        })),
    );
    let report = manager.sweep().await.expect("sweep");
    assert_eq!(report.terminalized, vec!["task_exhausted".to_string()]);
    let notification = &notifier.notifications()[0];
    assert_eq!(notification.status, BackgroundTaskStatus::Interrupted);
    assert_eq!(
        notification.reason.as_deref(),
        Some("resume_attempts_exhausted")
    );
}

#[tokio::test]
async fn sweep_reemits_terminal_notification_that_was_not_delivered() {
    let temp = tempfile::tempdir().expect("temp");
    let journal = Arc::new(MemoryTaskJournal::new());
    let notifier = Arc::new(RecordingNotifier::new());
    let record = running_record("task_notify", temp.path().join("n.output"), 1);
    journal.save(record.clone()).await.expect("save");
    let terminal = BackgroundTaskTerminal {
        status: BackgroundTaskStatus::Failed,
        reason: Some("boom".to_string()),
        exit_code: Some(2),
        usage: TaskUsage::default(),
        acceptance: None,
        finished_at_ms: 7,
    };
    let finished = journal
        .finish("task_notify", terminal, 0, false)
        .await
        .expect("finish")
        .expect("winner");
    assert!(!finished.notified);

    let manager = manager(
        &temp.path().join("outputs"),
        journal.clone(),
        notifier.clone(),
        1024,
    );
    let report = manager.sweep().await.expect("sweep");
    assert_eq!(report.renotified, vec!["task_notify".to_string()]);
    assert_eq!(notifier.len(), 1);
    assert_eq!(
        notifier.notifications()[0].status,
        BackgroundTaskStatus::Failed
    );

    // 已通知后不再重发。
    let mut updated = finished;
    updated.notified = true;
    journal.save(updated).await.expect("overwrite");
    let second = manager.sweep().await.expect("second");
    assert!(second.renotified.is_empty());
    assert_eq!(notifier.len(), 1);
}

/// 完成与取消（或 resume）可能并发结算同一任务：只有赢家能追加输出，输家必须
/// 在写文件前被 CAS 挡下，记录里的 `output_bytes` 也只能反映赢家内容。
#[tokio::test]
async fn concurrent_completions_append_only_the_winner_output() {
    let temp = tempfile::tempdir().expect("temp");
    let journal = Arc::new(MemoryTaskJournal::new());
    let notifier = Arc::new(RecordingNotifier::new());
    let manager = manager(
        &temp.path().join("outputs"),
        journal.clone(),
        notifier.clone(),
        1_048_576,
    );
    // 保持 sender 存活，让任务停在中途（否则 drop 会让 oneshot 返回并提前完成）。
    let (_release, wait) = oneshot::channel::<()>();
    let handle = manager
        .start(
            BackgroundTaskSpec::new("parent", "turn", "bash"),
            move |_context| async move {
                let _ = wait.await;
                TaskOutcome::completed("late")
            },
        )
        .await
        .expect("start");

    let (first, second) = tokio::join!(
        manager.finish_task(&handle.task_id, TaskOutcome::completed("first\n")),
        manager.finish_task(&handle.task_id, TaskOutcome::completed("second\n")),
    );
    let first = first.expect("first finish");
    let second = second.expect("second finish");
    assert!(
        first.won_race != second.won_race,
        "exactly one completion wins"
    );

    let winner_output = if first.won_race {
        "first\n"
    } else {
        "second\n"
    };
    let written = fs::read_to_string(&handle.output_path).expect("read output");
    assert_eq!(
        written, winner_output,
        "the losing completion must not append its output"
    );
    let record = manager
        .task(&handle.task_id)
        .await
        .expect("task")
        .expect("record");
    assert_eq!(record.output_bytes, winner_output.len() as u64);
}

#[tokio::test]
async fn sweep_skips_tasks_that_are_live_in_this_process() {
    let temp = tempfile::tempdir().expect("temp");
    let journal = Arc::new(MemoryTaskJournal::new());
    let notifier = Arc::new(RecordingNotifier::new());
    let manager_a = manager(
        &temp.path().join("a"),
        journal.clone(),
        notifier.clone(),
        1024,
    );
    // 让任务停在“运行中”：sweep 必须能看出它属于本进程里仍然存活的执行者。
    let (release, wait) = oneshot::channel::<()>();
    let handle = manager_a
        .start(
            BackgroundTaskSpec::new("session-a", "turn", "bash"),
            move |_context| async move {
                let _ = wait.await;
                TaskOutcome::completed("done\n")
            },
        )
        .await
        .expect("start");

    // 同一进程里的另一个会话共享同一份 journal，对全局记录做 sweep（None 形式，
    // 覆盖崩溃接管的场景）。只看 pid 会把 A 的任务当成进程丢失。
    let manager_b = manager(
        &temp.path().join("b"),
        journal.clone(),
        notifier.clone(),
        1024,
    );
    let report = manager_b.sweep().await.expect("sweep");
    assert!(
        report.skipped_live.contains(&handle.task_id),
        "a live in-process task must be skipped: {report:?}"
    );
    assert!(report.terminalized.is_empty(), "{report:?}");
    let record = journal
        .load(&handle.task_id)
        .await
        .expect("load")
        .expect("record");
    assert!(
        !record.is_terminal(),
        "the live task must keep its Running record"
    );

    release.send(()).expect("release");
    wait_for_notifications(&notifier, 1).await;
}
