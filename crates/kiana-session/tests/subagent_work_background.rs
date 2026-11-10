//! Phase 1c：后台 Work。
//!
//! 覆盖：
//! - 后台入口立即返回 task/Work 身份，Work 在会话的 LocalSet 上完成并写两条账；
//! - 预检失败在创建后台任务之前同步返回；
//! - 启动恢复把 Work 账本的权威终态对账回 background attempt（成功与可恢复两条路径）。

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message,
};
use kiana_session::background::{
    BackgroundTaskJournal, BackgroundTaskManager, BackgroundTaskRecord, BackgroundTaskStatus,
    DurableTaskJournal,
};
use kiana_session::continuity::ContinuityOptions;
use kiana_session::subagent::{
    MemoryWorkStore, RunId, SubagentRuntimeOptions, WorkId, WorkOptions, WorkRecord, WorkRecovery,
    WorkResult, WorkStore, WorkTerminal, WorkUsage, build_work_request,
};
use serde_json::json;

fn background_continuity(state_dir: &std::path::Path) -> ContinuityOptions {
    ContinuityOptions {
        execution: false,
        checkpoints: false,
        ..ContinuityOptions::new(state_dir)
    }
}

fn general_purpose() -> kiana_session::agent_definition::AgentDefinition {
    kiana_session::agent_definition::builtin_agent_definitions()
        .into_iter()
        .find(|definition| definition.name == "general-purpose")
        .expect("builtin general-purpose")
}

fn text_message(text: &str) -> FauxResponseStep {
    FauxResponseStep::Message(faux_assistant_message(
        text,
        FauxAssistantMessageOptions::default(),
    ))
}

async fn wait_for_terminal(
    manager: &Arc<BackgroundTaskManager>,
    task_id: &str,
) -> BackgroundTaskRecord {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let record = manager.task(task_id).await.expect("task").expect("record");
            if record.is_terminal() {
                return record;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("background task must reach a terminal state")
}

fn running_journal_record(
    task_id: &str,
    parent_session_id: &str,
    output_path: &std::path::Path,
) -> BackgroundTaskRecord {
    BackgroundTaskRecord {
        version: 1,
        task_id: task_id.to_string(),
        parent_session_id: parent_session_id.to_string(),
        parent_turn_id: "work:test".to_string(),
        tool: "agent".to_string(),
        description: None,
        status: BackgroundTaskStatus::Running,
        terminal_seq: 0,
        terminal: None,
        output_path: output_path.to_string_lossy().into_owned(),
        output_bytes: 0,
        output_truncated: false,
        notified: false,
        attempts: 1,
        created_at_ms: 1,
        updated_at_ms: 1,
        pid: None,
        // 模拟后台 Work attempt：sweep 不能替 Work 账本宣告终态。
        work_backed: true,
        work_id: None,
        run_id: None,
    }
}

#[tokio::test]
async fn background_work_links_the_task_and_work_ledgers() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(background_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![text_message("background done")]);
        let parent_session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");
        let manager = harness
            .session
            .background_task_manager()
            .expect("background manager")
            .clone();

        let handle = harness
            .session
            .spawn_subagent_with_work_in_background(
                general_purpose(),
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "do the work in the background",
                None,
                WorkOptions {
                    store: Some(store.clone()),
                    ..Default::default()
                },
            )
            .await
            .expect("background work");
        assert!(handle.task_id.starts_with("task_"));
        assert!(handle.output_path.exists());

        let task = wait_for_terminal(&manager, &handle.task_id).await;
        assert_eq!(task.status, BackgroundTaskStatus::Completed);

        let work = store
            .load(&parent_session_id, handle.work_id)
            .await
            .expect("load")
            .expect("work record");
        assert!(work.is_terminal());
        assert_eq!(work.run_id, handle.run_id);
        assert_eq!(
            work.background_task_id.as_deref(),
            Some(handle.task_id.as_str()),
            "the Work record must link its background attempt"
        );
        assert_eq!(
            work.terminal_result().expect("terminal").terminal.status(),
            "success"
        );

        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn cancelling_a_background_work_writes_a_cancelled_work_terminal() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(background_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        // 模型永不按时返回：只有取消能结束这次运行。
        harness.set_responses(vec![FauxResponseStep::factory(|_input| async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            Ok(faux_assistant_message("late", Default::default()))
        })]);
        let parent_session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");
        let manager = harness
            .session
            .background_task_manager()
            .expect("background manager")
            .clone();

        let handle = harness
            .session
            .spawn_subagent_with_work_in_background(
                general_purpose(),
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "cancel the background work",
                None,
                WorkOptions {
                    store: Some(store.clone()),
                    ..Default::default()
                },
            )
            .await
            .expect("background work");

        assert!(
            harness
                .session
                .cancel_background_task(&handle.task_id)
                .await
                .expect("cancel"),
            "a running background task must be cancellable"
        );
        // 协作式取消：task 终态由 Work 生命周期写完后映射，不在这里抢写。
        let task = wait_for_terminal(&manager, &handle.task_id).await;
        assert_eq!(task.status, BackgroundTaskStatus::Cancelled);

        // 协作式取消不 abort Work 生命周期：Work 账本必须自己写出 Cancelled。
        let record = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let record = store
                    .load(&parent_session_id, handle.work_id)
                    .await
                    .expect("load");
                if record.as_ref().is_some_and(WorkRecord::is_terminal) {
                    break record.expect("work record");
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the Work lifecycle must finish after cancellation");
        assert_eq!(
            record
                .terminal_result()
                .expect("terminal result")
                .terminal
                .status(),
            "cancelled"
        );

        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn background_work_preflight_fails_before_creating_a_task() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(background_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let manager = harness
            .session
            .background_task_manager()
            .expect("background manager")
            .clone();

        // resumable 但没有持久 session_dir：与前台路径同一预检，必须在创建后台
        // 任务之前失败。
        let error = harness
            .session
            .spawn_subagent_with_work_in_background(
                general_purpose(),
                SubagentRuntimeOptions::default(),
                "resume me",
                None,
                WorkOptions {
                    recovery: WorkRecovery::Resume,
                    ..Default::default()
                },
            )
            .await
            .expect_err("preflight must fail");
        assert!(
            error
                .to_string()
                .contains("resumable work requires a persistent subagent session directory"),
            "{error}"
        );
        assert!(
            manager
                .list(&harness.session.session_id())
                .await
                .expect("list")
                .is_empty(),
            "a failed preflight must not create a background task"
        );

        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn background_work_requires_the_background_manager() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        let error = harness
            .session
            .spawn_subagent_with_work_in_background(
                general_purpose(),
                SubagentRuntimeOptions::default(),
                "background",
                None,
                WorkOptions::default(),
            )
            .await
            .expect_err("background manager is required");
        assert!(
            error.to_string().contains("continuity.background"),
            "{error}"
        );
        harness.cleanup();
    })
    .await;
}

/// 回归（Phase 1c 设计评审）：后台 Work 必须落在 durable Work 账本上，否则启动
/// 对账没有任何权威记录可读，attempt 崩溃后只能是不可恢复的孤儿。
#[tokio::test]
async fn background_work_requires_a_durable_work_store() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(ContinuityOptions {
                execution: false,
                checkpoints: false,
                work: false,
                ..ContinuityOptions::new(state_dir)
            }),
            ..Default::default()
        })
        .await;

        let error = harness
            .session
            .spawn_subagent_with_work_in_background(
                general_purpose(),
                SubagentRuntimeOptions::default(),
                "background",
                None,
                WorkOptions::default(),
            )
            .await
            .expect_err("background work requires a durable Work ledger");
        assert!(error.to_string().contains("durable work store"), "{error}");
        let manager = harness
            .session
            .background_task_manager()
            .expect("background manager")
            .clone();
        assert!(
            manager
                .list(&harness.session.session_id())
                .await
                .expect("list")
                .is_empty(),
            "the rejected spawn must not create a background task"
        );
        harness.cleanup();
    })
    .await;
}

/// 回归（Phase 1c 设计评审 finding 15）：崩溃恢复落定的 Work 是
/// `Failed { stopReason: "interrupted" }`，后台 attempt 必须映射成 Interrupted，
/// 不能退化成普通 Failed。
#[tokio::test]
async fn recover_work_maps_a_process_lost_attempt_to_interrupted() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(background_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let parent_session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");
        let manager = harness
            .session
            .background_task_manager()
            .expect("manager")
            .clone();

        let task_id = "task_reconcile_interrupted";
        let output_path = state_dir
            .join("background")
            .join(format!("{task_id}.output"));
        let journal = DurableTaskJournal::new(state_dir.join("background"));
        journal
            .save(running_journal_record(
                task_id,
                &parent_session_id,
                &output_path,
            ))
            .await
            .expect("journal record");

        let request = build_work_request(
            &general_purpose(),
            &SubagentRuntimeOptions::default(),
            "interrupted background attempt",
            state.path(),
            &WorkOptions {
                work_id: Some(WorkId(902)),
                run_id: Some(RunId::new("run-902")),
                ..Default::default()
            },
        );
        store
            .begin(
                WorkRecord::new(request.clone(), parent_session_id.clone(), None, None, 1)
                    .with_background_task_id(task_id),
            )
            .await
            .expect("begin");
        let result =
            WorkResult::interrupted(&request, "work interrupted: process lost", None, 1, 2);
        store
            .finish(&parent_session_id, &result)
            .await
            .expect("finish");

        harness.session.recover_work().await.expect("recover");

        let task = manager.task(task_id).await.expect("task").expect("record");
        assert_eq!(task.status, BackgroundTaskStatus::Interrupted);
        assert_eq!(
            task.terminal.expect("terminal").reason.as_deref(),
            Some("process_lost")
        );

        harness.cleanup();
    })
    .await;
}

/// 回归（Phase 1c 设计评审 finding 13）：同一 `(parentSessionId, workId)` 已存在时
/// 在创建后台任务之前同步失败，handle 不会宣告一个不会开始的 Work。
#[tokio::test]
async fn background_work_rejects_a_duplicate_work_id_before_creating_the_task() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(background_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let parent_session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");
        let manager = harness
            .session
            .background_task_manager()
            .expect("manager")
            .clone();

        let request = build_work_request(
            &general_purpose(),
            &SubagentRuntimeOptions::default(),
            "existing",
            state.path(),
            &WorkOptions {
                work_id: Some(WorkId(903)),
                run_id: Some(RunId::new("run-903")),
                ..Default::default()
            },
        );
        store
            .begin(WorkRecord::new(
                request,
                parent_session_id.clone(),
                None,
                None,
                1,
            ))
            .await
            .expect("begin");

        let error = harness
            .session
            .spawn_subagent_with_work_in_background(
                general_purpose(),
                SubagentRuntimeOptions::default(),
                "duplicate",
                None,
                WorkOptions {
                    work_id: Some(WorkId(903)),
                    run_id: Some(RunId::new("run-903b")),
                    ..Default::default()
                },
            )
            .await
            .expect_err("a duplicate WorkId must fail synchronously");
        assert!(error.to_string().contains("already exists"), "{error}");
        assert!(
            manager
                .list(&parent_session_id)
                .await
                .expect("list")
                .is_empty(),
            "the rejected spawn must not create a background task"
        );
        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn recover_work_reconciles_a_finished_background_attempt() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(background_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let parent_session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");
        let manager = harness
            .session
            .background_task_manager()
            .expect("manager")
            .clone();

        // 模拟崩溃窗口：Work 已成功落定，background attempt 还停在 Running。
        let task_id = "task_reconcile_success";
        let output_path = state_dir
            .join("background")
            .join(format!("{task_id}.output"));
        let journal = DurableTaskJournal::new(state_dir.join("background"));
        journal
            .save(running_journal_record(
                task_id,
                &parent_session_id,
                &output_path,
            ))
            .await
            .expect("journal record");

        let request = build_work_request(
            &general_purpose(),
            &SubagentRuntimeOptions::default(),
            "background attempt",
            state.path(),
            &WorkOptions {
                work_id: Some(WorkId(900)),
                run_id: Some(RunId::new("run-900")),
                ..Default::default()
            },
        );
        store
            .begin(
                WorkRecord::new(request.clone(), parent_session_id.clone(), None, None, 1)
                    .with_background_task_id(task_id),
            )
            .await
            .expect("begin");
        let mut result = WorkResult::failure(&request, "placeholder", 1, 2);
        result.terminal = WorkTerminal::Success {
            output: Some(json!({"ok": true})),
        };
        result.usage = WorkUsage {
            input_tokens: 7,
            output_tokens: 3,
            total_tokens: 10,
            ..WorkUsage::default()
        };
        store
            .finish(&parent_session_id, &result)
            .await
            .expect("finish");

        harness.session.recover_work().await.expect("recover");

        let task = manager.task(task_id).await.expect("task").expect("record");
        assert_eq!(
            task.status,
            BackgroundTaskStatus::Completed,
            "the Work ledger is authoritative for a finished attempt"
        );
        assert_eq!(task.terminal.expect("terminal").usage.total_tokens, 10);
        let output = std::fs::read_to_string(&output_path).expect("output");
        assert!(output.contains("{\"ok\":true}"), "{output}");

        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn recover_work_closes_a_resumable_background_attempt_as_interrupted() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(background_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let parent_session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");
        let manager = harness
            .session
            .background_task_manager()
            .expect("manager")
            .clone();

        let task_id = "task_reconcile_resumable";
        let output_path = state_dir
            .join("background")
            .join(format!("{task_id}.output"));
        let journal = DurableTaskJournal::new(state_dir.join("background"));
        journal
            .save(running_journal_record(
                task_id,
                &parent_session_id,
                &output_path,
            ))
            .await
            .expect("journal record");

        let request = build_work_request(
            &general_purpose(),
            &SubagentRuntimeOptions::default(),
            "resumable background attempt",
            state.path(),
            &WorkOptions {
                work_id: Some(WorkId(901)),
                run_id: Some(RunId::new("run-901")),
                recovery: WorkRecovery::Resume,
                ..Default::default()
            },
        );
        store
            .begin(
                WorkRecord::new(request, parent_session_id.clone(), None, None, 1)
                    .with_background_task_id(task_id),
            )
            .await
            .expect("begin");
        store
            .attach_session(
                &parent_session_id,
                WorkId(901),
                &RunId::new("run-901"),
                "/tmp/resumable-child.jsonl",
            )
            .await
            .expect("attach");

        let report = harness
            .session
            .recover_work()
            .await
            .expect("recover")
            .expect("report");
        assert_eq!(
            report.resumable,
            vec![WorkId(901)],
            "recovery=resume keeps the Work record resumable"
        );

        let work = store
            .load(&parent_session_id, WorkId(901))
            .await
            .expect("load")
            .expect("work");
        assert!(!work.is_terminal(), "the Work record stays resumable");
        let task = manager.task(task_id).await.expect("task").expect("record");
        assert_eq!(
            task.status,
            BackgroundTaskStatus::Interrupted,
            "the dead attempt is closed even though the Work can be resumed"
        );

        harness.cleanup();
    })
    .await;
}

/// 回归（round-19 finding 2）：journal 已写 Running、`WorkStore::begin` 尚未执行时
/// 进程崩溃。记录里有 workId/runId，但 Work 账本没有对应记录；启动对账必须关闭
/// 这个 attempt，不能让它永远停在 Running。
#[tokio::test]
async fn recover_work_closes_an_attempt_whose_work_never_began() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(background_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let parent_session_id = harness.session.session_id();
        let manager = harness
            .session
            .background_task_manager()
            .expect("manager")
            .clone();

        let task_id = "task_never_began";
        let output_path = state_dir
            .join("background")
            .join(format!("{task_id}.output"));
        let journal = DurableTaskJournal::new(state_dir.join("background"));
        let mut record = running_journal_record(task_id, &parent_session_id, &output_path);
        record.work_id = Some(904);
        record.run_id = Some("run-904".to_string());
        journal.save(record).await.expect("journal record");

        harness.session.recover_work().await.expect("recover");

        let task = manager.task(task_id).await.expect("task").expect("record");
        assert_eq!(task.status, BackgroundTaskStatus::Interrupted);
        harness.cleanup();
    })
    .await;
}

/// 回归（round-19 finding 5）：后台恢复只读会话 durable 账本；注入其它 store
/// 会让 begin 的 Work 记录在崩溃后无法被 recover/reconcile 找到。
#[tokio::test]
async fn background_work_rejects_a_foreign_work_store() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(background_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let foreign: Arc<dyn WorkStore> = Arc::new(MemoryWorkStore::new());
        let error = harness
            .session
            .spawn_subagent_with_work_in_background(
                general_purpose(),
                SubagentRuntimeOptions::default(),
                "background with a foreign store",
                None,
                WorkOptions {
                    store: Some(foreign),
                    ..Default::default()
                },
            )
            .await
            .expect_err("a foreign work store cannot back background recovery");
        assert!(
            error.to_string().contains("session's durable work store"),
            "{error}"
        );
        let manager = harness
            .session
            .background_task_manager()
            .expect("manager")
            .clone();
        assert!(
            manager
                .list(&harness.session.session_id())
                .await
                .expect("list")
                .is_empty(),
            "the rejected spawn must not create a background task"
        );
        harness.cleanup();
    })
    .await;
}
