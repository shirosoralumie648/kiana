//! Phase 2：声明式 stage 流水线的端到端测试。
//!
//! 覆盖：
//! - 顺序执行 plan → implement → verify，artifact 通过提示词传递；
//! - `review.json` 按 workId 命名空间化并严格回读；
//! - stage 没有产出必需 artifact 时 Work 失败；
//! - review 写入/回读失败时整个 Work 被判 reject；
//! - staged Work 拒绝 resumable 与隔离工作区启动。

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::*;
use kiana_agent_core::types::{
    AgentTool, AgentToolError, AgentToolExecute, AgentToolResult, BoxFuture,
};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message, faux_tool_call,
};
use kiana_ai::types::{JsonObject, JsonValue, StopReason, TextContent, TextKind, UserContentBlock};
use kiana_session::acceptance::AcceptanceStatus;
use kiana_session::agent_definition::{
    AgentDefinition, AgentStage, AgentStageBudget, StageArtifact, builtin_agent_definitions,
};
use kiana_session::continuity::ContinuityOptions;
use kiana_session::subagent::stages::{load_review, review_path};
use kiana_session::subagent::{
    IsolationMode, RunId, SubagentRuntimeOptions, WorkId, WorkOptions, WorkRecovery, WorkTerminal,
};
use serde_json::{Value, json};

fn tool_call_message(name: &str, arguments: Value) -> FauxResponseStep {
    let arguments: JsonObject = serde_json::from_value(arguments).expect("object");
    FauxResponseStep::Message(faux_assistant_message(
        faux_tool_call(name, arguments),
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        },
    ))
}

fn text_message(text: &str) -> FauxResponseStep {
    FauxResponseStep::Message(faux_assistant_message(text, Default::default()))
}

fn work_continuity(state_dir: &std::path::Path) -> ContinuityOptions {
    ContinuityOptions {
        execution: false,
        checkpoints: false,
        background: false,
        ..ContinuityOptions::new(state_dir)
    }
}

fn general_purpose() -> AgentDefinition {
    builtin_agent_definitions()
        .into_iter()
        .find(|definition| definition.name == "general-purpose")
        .expect("builtin general-purpose")
}

fn stage(name: &str, consumes: &[StageArtifact], produces: &[StageArtifact]) -> AgentStage {
    AgentStage {
        name: name.to_string(),
        owner: None,
        consumes: consumes.to_vec(),
        produces: produces.to_vec(),
        exit_criteria: Vec::new(),
        budget: AgentStageBudget::default(),
    }
}

fn staged_definition() -> AgentDefinition {
    let mut definition = general_purpose();
    let mut plan = stage("plan", &[], &[StageArtifact::Plan]);
    // 跨 agent 的 owner：内置 `explore` 是只读定义，yield 工具仍由 runner 追加。
    plan.owner = Some("explore".to_string());
    definition.stages = Some(vec![
        plan,
        stage(
            "implement",
            &[StageArtifact::Plan],
            &[StageArtifact::ChangedFiles],
        ),
        stage(
            "verify",
            &[StageArtifact::ChangedFiles],
            &[StageArtifact::ReviewFindings],
        ),
    ]);
    definition
}

#[tokio::test]
async fn a_sequential_workflow_runs_each_stage_and_persists_the_review() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state.path().join("continuity"))),
            ..Default::default()
        })
        .await;
        let session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");
        let findings = json!([{"severity": "info", "summary": "looks good"}]);
        harness.set_responses(vec![
            tool_call_message("yield", json!({"plan": {"steps": ["read", "edit"]}})),
            text_message("planned"),
            tool_call_message("yield", json!({"changed_files": ["src/lib.rs"]})),
            text_message("implemented"),
            tool_call_message("yield", json!({"review_findings": findings.clone()})),
            text_message("verified"),
        ]);

        let work = harness
            .session
            .spawn_subagent_with_work(
                staged_definition(),
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "ship the change",
                None,
                WorkOptions {
                    work_id: Some(WorkId(11)),
                    run_id: Some(RunId::new("run-11")),
                    store: Some(store.clone()),
                    ..Default::default()
                },
            )
            .await
            .expect("staged work");

        assert_eq!(work.result.terminal.status(), "success");
        let output = work.outcome.output.expect("artifact output");
        assert_eq!(output["plan"], json!({"steps": ["read", "edit"]}));
        assert_eq!(output["changed_files"], json!(["src/lib.rs"]));
        assert_eq!(output["review_findings"], findings);

        // 最后一个 stage 的提示词带上了它消费的 artifact。
        let messages = serde_json::to_string(&work.outcome.messages).expect("messages");
        assert!(
            messages.contains("Workflow stage 3/3: verify"),
            "last stage prompt must be the verify stage"
        );
        assert!(
            messages.contains("changed_files"),
            "verify stage must receive changed_files from the previous stage"
        );

        // review.json 按 workId 命名空间化，并记录当前 run。
        let path = review_path(
            harness.temp_dir.path(),
            &session_id,
            WorkId(11),
            &RunId::new("run-11"),
            "verify",
        );
        assert!(
            path.exists(),
            "review.json must be written: {}",
            path.display()
        );
        let stored = load_review(
            &path,
            WorkId(11),
            &RunId::new("run-11"),
            &stage("verify", &[], &[StageArtifact::ReviewFindings]),
            0,
        )
        .expect("review must round-trip");
        assert_eq!(stored, findings);

        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn a_stage_that_does_not_yield_its_artifact_fails_the_work() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state.path().join("continuity"))),
            ..Default::default()
        })
        .await;
        let store = harness.session.durable_work_store().expect("work store");
        let mut definition = general_purpose();
        definition.stages = Some(vec![stage("plan", &[], &[StageArtifact::Plan])]);
        // 模型直接给出文本、从未 yield：runner 视为“没提交结构化结果”，
        // stage 的 artifact 收集必须把 Work 判失败。
        harness.set_responses(vec![text_message("no artifacts")]);

        let work = harness
            .session
            .spawn_subagent_with_work(
                definition,
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "plan the change",
                None,
                WorkOptions {
                    work_id: Some(WorkId(12)),
                    run_id: Some(RunId::new("run-12")),
                    store: Some(store),
                    ..Default::default()
                },
            )
            .await
            .expect("work returns a terminal result");

        assert_eq!(work.result.terminal.status(), "failed");
        match &work.result.terminal {
            WorkTerminal::Failed { error, .. } => {
                assert!(
                    error.contains("did not yield required artifact `plan`"),
                    "{error}"
                );
            }
            other => panic!("expected failed terminal, got {other:?}"),
        }

        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn an_unwritable_review_file_rejects_the_work() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state.path().join("continuity"))),
            ..Default::default()
        })
        .await;
        let session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");
        // 预先占用 review.json 的路径（目录无法被 rename 覆盖），模拟写入失败。
        let path = review_path(
            harness.temp_dir.path(),
            &session_id,
            WorkId(13),
            &RunId::new("run-13"),
            "verify",
        );
        std::fs::create_dir_all(&path).expect("directory at review path");
        harness.set_responses(vec![
            tool_call_message("yield", json!({"review_findings": []})),
            text_message("verified"),
        ]);
        let mut definition = general_purpose();
        definition.stages = Some(vec![stage("verify", &[], &[StageArtifact::ReviewFindings])]);

        let work = harness
            .session
            .spawn_subagent_with_work(
                definition,
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "verify the change",
                None,
                WorkOptions {
                    work_id: Some(WorkId(13)),
                    run_id: Some(RunId::new("run-13")),
                    store: Some(store),
                    ..Default::default()
                },
            )
            .await
            .expect("work returns a terminal result");

        assert_eq!(work.result.terminal.status(), "failed");
        match &work.result.terminal {
            WorkTerminal::Failed { error, .. } => {
                assert!(error.starts_with("review rejected:"), "{error}");
            }
            other => panic!("expected failed terminal, got {other:?}"),
        }

        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn staged_work_rejects_resumable_and_isolated_launches() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state.path().join("continuity"))),
            ..Default::default()
        })
        .await;

        let error = harness
            .session
            .spawn_subagent_with_work(
                staged_definition(),
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "ship the change",
                None,
                WorkOptions {
                    recovery: WorkRecovery::Resume,
                    ..Default::default()
                },
            )
            .await
            .expect_err("staged work cannot be resumed");
        assert!(error.to_string().contains("not resumable"), "{error}");

        // 普通路径不能静默只跑第一阶段。
        let error = harness
            .session
            .spawn_subagent(
                staged_definition(),
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "ship the change",
                None,
            )
            .await
            .expect_err("staged agents require the Work path");
        assert!(error.to_string().contains("declares stages"), "{error}");

        let mut isolated = staged_definition();
        isolated.isolation = kiana_session::subagent::IsolationMode::Worktree;
        let error = harness
            .session
            .spawn_subagent_with_work(
                isolated,
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "ship the change",
                None,
                WorkOptions::default(),
            )
            .await
            .expect_err("staged work requires a shared workspace");
        assert!(error.to_string().contains("isolation=shared"), "{error}");

        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn a_stage_exit_criterion_failure_fails_the_work() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state.path().join("continuity"))),
            ..Default::default()
        })
        .await;
        let store = harness.session.durable_work_store().expect("work store");
        let mut definition = general_purpose();
        let mut plan = stage("plan", &[], &[StageArtifact::Plan]);
        plan.exit_criteria = vec!["file:never-written.txt exists".to_string()];
        definition.stages = Some(vec![plan]);
        harness.set_responses(vec![
            tool_call_message("yield", json!({"plan": {"steps": ["a"]}})),
            text_message("done"),
        ]);

        let work = harness
            .session
            .spawn_subagent_with_work(
                definition,
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "plan the change",
                None,
                WorkOptions {
                    work_id: Some(WorkId(14)),
                    run_id: Some(RunId::new("run-14")),
                    store: Some(store),
                    ..Default::default()
                },
            )
            .await
            .expect("work returns a terminal result");

        assert_eq!(work.result.terminal.status(), "failed");
        match &work.result.terminal {
            WorkTerminal::Failed { error, .. } => {
                assert!(
                    error.contains("stage `plan` exit criteria failed"),
                    "{error}"
                );
                assert!(error.contains("never-written.txt"), "{error}");
            }
            other => panic!("expected failed terminal, got {other:?}"),
        }
        // 宿主没有声明 acceptance criteria：WorkResult 必须是 Work 级（Unverified），
        // 不能把 stage 自己的 exit-criteria 报告透出去。
        let acceptance = work
            .result
            .acceptance
            .as_ref()
            .expect("work-level acceptance report")
            .report
            .clone();
        assert_eq!(
            acceptance.status,
            kiana_session::acceptance::AcceptanceStatus::Unverified
        );
        assert!(acceptance.leaves.is_empty(), "{:?}", acceptance.leaves);

        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn a_stage_budget_timeout_marks_the_work_timed_out() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state.path().join("continuity"))),
            ..Default::default()
        })
        .await;
        let store = harness.session.durable_work_store().expect("work store");
        let mut definition = general_purpose();
        let mut slow = stage("plan", &[], &[StageArtifact::Plan]);
        slow.budget.timeout_seconds = Some(1);
        definition.stages = Some(vec![slow]);
        // 模型永不返回：只有 stage 定时器能把这次运行停下来。
        harness.set_responses(vec![FauxResponseStep::factory(|_input| async {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            Ok(faux_assistant_message("late", Default::default()))
        })]);

        let work = harness
            .session
            .spawn_subagent_with_work(
                definition,
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "plan the change",
                None,
                WorkOptions {
                    work_id: Some(WorkId(15)),
                    run_id: Some(RunId::new("run-15")),
                    store: Some(store),
                    ..Default::default()
                },
            )
            .await
            .expect("work returns a terminal result");

        assert_eq!(work.result.terminal.status(), "timed_out");
        match &work.result.terminal {
            WorkTerminal::TimedOut { reason } => {
                assert!(
                    reason
                        .as_deref()
                        .is_some_and(|reason| reason.contains("stage `plan` timed out after 1s")),
                    "{reason:?}"
                );
            }
            other => panic!("expected timed out terminal, got {other:?}"),
        }

        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn an_unverified_exit_criterion_fails_the_stage() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state.path().join("continuity"))),
            ..Default::default()
        })
        .await;
        let store = harness.session.durable_work_store().expect("work store");
        let mut definition = general_purpose();
        let mut plan = stage("plan", &[], &[StageArtifact::Plan]);
        // 无法用代码判定的 criterion：不能静默放行。
        plan.exit_criteria = vec!["looks reasonable".to_string()];
        definition.stages = Some(vec![plan]);
        harness.set_responses(vec![
            tool_call_message("yield", json!({"plan": {"steps": ["a"]}})),
            text_message("done"),
        ]);

        let work = harness
            .session
            .spawn_subagent_with_work(
                definition,
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "plan the change",
                None,
                WorkOptions {
                    work_id: Some(WorkId(16)),
                    run_id: Some(RunId::new("run-16")),
                    store: Some(store),
                    ..Default::default()
                },
            )
            .await
            .expect("work returns a terminal result");

        match &work.result.terminal {
            WorkTerminal::Failed { error, .. } => {
                assert!(
                    error.contains("stage `plan` exit criteria unverified"),
                    "{error}"
                );
                assert!(error.contains("looks reasonable"), "{error}");
            }
            other => panic!("expected failed terminal, got {other:?}"),
        }

        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn a_stage_owner_with_another_isolation_mode_is_rejected() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state.path().join("continuity"))),
            ..Default::default()
        })
        .await;
        let store = harness.session.durable_work_store().expect("work store");
        let agents = harness.temp_dir.path().join(".kiana").join("agents");
        std::fs::create_dir_all(&agents).expect("agents dir");
        std::fs::write(
            agents.join("heavy.md"),
            "---\nname: heavy\ndescription: Isolated worker\nisolation: worktree\n---\nWork in a worktree.\n",
        )
        .expect("agent file");
        let mut definition = general_purpose();
        let mut plan = stage("plan", &[], &[StageArtifact::Plan]);
        plan.owner = Some("heavy".to_string());
        definition.stages = Some(vec![plan]);

        let work = harness
            .session
            .spawn_subagent_with_work(
                definition,
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "plan the change",
                None,
                WorkOptions {
                    work_id: Some(WorkId(17)),
                    run_id: Some(RunId::new("run-17")),
                    store: Some(store),
                    ..Default::default()
                },
            )
            .await
            .expect("work returns a terminal result");

        match &work.result.terminal {
            WorkTerminal::Failed { error, .. } => {
                assert!(
                    error.contains("stage `plan` owner `heavy` requires isolation=worktree"),
                    "{error}"
                );
            }
            other => panic!("expected failed terminal, got {other:?}"),
        }

        harness.cleanup();
    })
    .await;
}

/// 回归（round-18 finding #4）：已取消的 staged Work 不能先解析下一个 stage 的
/// owner——owner 不存在会被合成为 Failed，把取消改写成失败终态。循环开头的取消
/// 检查必须先于 owner 加载。
#[tokio::test]
async fn a_pre_cancelled_staged_work_reports_cancelled_instead_of_failing_the_owner_lookup() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state.path().join("continuity"))),
            ..Default::default()
        })
        .await;
        let store = harness.session.durable_work_store().expect("work store");
        let mut definition = general_purpose();
        // owner 不存在：若取消检查排在 owner 加载之后，这里会得到 Failed。
        let mut plan = stage("plan", &[], &[StageArtifact::Plan]);
        plan.owner = Some("does-not-exist".to_string());
        definition.stages = Some(vec![plan]);

        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        let work = harness
            .session
            .spawn_subagent_with_work(
                definition,
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "plan the change",
                Some(cancel),
                WorkOptions {
                    work_id: Some(WorkId(21)),
                    run_id: Some(RunId::new("run-21")),
                    store: Some(store),
                    ..Default::default()
                },
            )
            .await
            .expect("work returns a terminal result");

        match &work.result.terminal {
            WorkTerminal::Cancelled { .. } => {}
            other => panic!("expected cancelled terminal, got {other:?}"),
        }

        harness.cleanup();
    })
    .await;
}

/// 回归（round-18 finding #8）：运行时 isolation override 优先于 owner 声明。定义
/// 声明 worktree、运行时显式 shared 时，预检与每个 stage 必须用同一个有效值，
/// 不能通过预检后在第一个 stage 才失败。
#[tokio::test]
async fn a_stage_owner_with_worktree_isolation_accepts_a_runtime_shared_override() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state.path().join("continuity"))),
            ..Default::default()
        })
        .await;
        let store = harness.session.durable_work_store().expect("work store");
        let agents = harness.temp_dir.path().join(".kiana").join("agents");
        std::fs::create_dir_all(&agents).expect("agents dir");
        std::fs::write(
            agents.join("heavy.md"),
            "---\nname: heavy\ndescription: Isolated worker\nisolation: worktree\n---\nWork in a worktree.\n",
        )
        .expect("agent file");
        let mut definition = general_purpose();
        let mut plan = stage("plan", &[], &[StageArtifact::Plan]);
        plan.owner = Some("heavy".to_string());
        definition.stages = Some(vec![plan]);
        harness.set_responses(vec![
            tool_call_message("yield", json!({"plan": {"steps": ["a"]}})),
            text_message("planned"),
        ]);

        let work = harness
            .session
            .spawn_subagent_with_work(
                definition,
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    isolation: Some(IsolationMode::Shared),
                    ..Default::default()
                },
                "plan the change",
                None,
                WorkOptions {
                    work_id: Some(WorkId(22)),
                    run_id: Some(RunId::new("run-22")),
                    store: Some(store),
                    ..Default::default()
                },
            )
            .await
            .expect("work returns a terminal result");

        assert_eq!(
            work.result.terminal.status(),
            "success",
            "the runtime shared override must apply to every stage"
        );

        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn a_staged_work_respects_the_work_level_timeout() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state.path().join("continuity"))),
            ..Default::default()
        })
        .await;
        let store = harness.session.durable_work_store().expect("work store");
        // stage 没有自己的预算；Work 级 timeoutSeconds 必须仍然生效。
        let mut definition = general_purpose();
        definition.stages = Some(vec![stage("plan", &[], &[StageArtifact::Plan])]);
        harness.set_responses(vec![FauxResponseStep::factory(|_input| async {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            Ok(faux_assistant_message("late", Default::default()))
        })]);

        let work = harness
            .session
            .spawn_subagent_with_work(
                definition,
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "plan the change",
                None,
                WorkOptions {
                    work_id: Some(WorkId(18)),
                    run_id: Some(RunId::new("run-18")),
                    timeout_seconds: Some(1),
                    store: Some(store),
                    ..Default::default()
                },
            )
            .await
            .expect("work returns a terminal result");

        assert_eq!(work.result.terminal.status(), "timed_out");

        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn a_review_is_persisted_even_when_a_later_stage_fails() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state.path().join("continuity"))),
            ..Default::default()
        })
        .await;
        let session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");
        let findings = json!([{"severity": "error", "summary": "fix the change"}]);
        let mut definition = general_purpose();
        definition.stages = Some(vec![
            stage("verify", &[], &[StageArtifact::ReviewFindings]),
            stage("implement", &[], &[StageArtifact::ChangedFiles]),
        ]);
        // verify 成功；implement 没有脚本可消费，faux provider 会报错让该 stage 失败。
        harness.set_responses(vec![
            tool_call_message("yield", json!({"review_findings": findings.clone()})),
            text_message("reviewed"),
        ]);

        let work = harness
            .session
            .spawn_subagent_with_work(
                definition,
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "verify and implement",
                None,
                WorkOptions {
                    work_id: Some(WorkId(19)),
                    run_id: Some(RunId::new("run-19")),
                    store: Some(store),
                    ..Default::default()
                },
            )
            .await
            .expect("work returns a terminal result");

        assert_eq!(work.result.terminal.status(), "failed");
        let path = review_path(
            harness.temp_dir.path(),
            &session_id,
            WorkId(19),
            &RunId::new("run-19"),
            "verify",
        );
        assert!(
            path.exists(),
            "review.json must be persisted before a later stage fails: {}",
            path.display()
        );
        assert_eq!(
            load_review(
                &path,
                WorkId(19),
                &RunId::new("run-19"),
                &stage("verify", &[], &[StageArtifact::ReviewFindings]),
                0,
            )
            .expect("review round-trip"),
            findings
        );

        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn a_stage_preparation_failure_keeps_earlier_stage_results() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state.path().join("continuity"))),
            ..Default::default()
        })
        .await;
        let store = harness.session.durable_work_store().expect("work store");
        let mut definition = general_purpose();
        let mut second = stage(
            "implement",
            &[StageArtifact::Plan],
            &[StageArtifact::ChangedFiles],
        );
        second.owner = Some("does-not-exist".to_string());
        definition.stages = Some(vec![stage("plan", &[], &[StageArtifact::Plan]), second]);
        harness.set_responses(vec![
            tool_call_message("yield", json!({"plan": {"steps": ["a"]}})),
            text_message("planned"),
        ]);

        let work = harness
            .session
            .spawn_subagent_with_work(
                definition,
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "plan and implement",
                None,
                WorkOptions {
                    work_id: Some(WorkId(20)),
                    run_id: Some(RunId::new("run-20")),
                    store: Some(store),
                    ..Default::default()
                },
            )
            .await
            .expect("work returns a terminal result");

        match &work.result.terminal {
            WorkTerminal::Failed { error, .. } => {
                assert!(error.contains("does-not-exist"), "{error}");
            }
            other => panic!("expected failed terminal, got {other:?}"),
        }
        assert!(
            !work.outcome.messages.is_empty(),
            "the completed plan stage's transcript must be preserved"
        );
        assert_eq!(
            work.result
                .acceptance
                .as_ref()
                .expect("work-level acceptance")
                .report
                .status,
            kiana_session::acceptance::AcceptanceStatus::Unverified
        );

        harness.cleanup();
    })
    .await;
}

/// 按调用序号切换输出的 shell 工具：前两次通过，之后失败。
struct ScriptedShellExecute {
    calls: Arc<AtomicUsize>,
}

impl AgentToolExecute for ScriptedShellExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: JsonValue,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        Box::pin(async move {
            let text = if self.calls.fetch_add(1, Ordering::SeqCst) < 2 {
                "test result: ok. 1 passed; 0 failed; 0 ignored"
            } else {
                "test result: FAILED. 0 passed; 1 failed; 0 ignored"
            };
            Ok(AgentToolResult {
                content: vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: text.to_string(),
                    text_signature: None,
                })],
                details: JsonValue::Null,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

fn scripted_shell_tool(calls: Arc<AtomicUsize>) -> AgentTool {
    AgentTool {
        name: "bash".to_string(),
        label: "Bash".to_string(),
        description: "run a shell command".to_string(),
        parameters: json!({"type": "object"}),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(ScriptedShellExecute { calls }),
    }
}

#[tokio::test]
async fn execution_sequences_stay_monotonic_across_stages() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state.path().join("continuity"))),
            tools: Some(vec![scripted_shell_tool(Arc::new(AtomicUsize::new(0)))]),
            ..Default::default()
        })
        .await;
        let store = harness.session.durable_work_store().expect("work store");
        let mut definition = general_purpose();
        definition.stages = Some(vec![
            stage("implement", &[], &[StageArtifact::ChangedFiles]),
            stage("verify", &[], &[StageArtifact::ReviewFindings]),
        ]);
        harness.set_responses(vec![
            // implement：两次通过的测试执行（seq 1、2）。
            tool_call_message("bash", json!({"command": "cargo test"})),
            tool_call_message("bash", json!({"command": "cargo test"})),
            tool_call_message("yield", json!({"changed_files": ["src/lib.rs"]})),
            text_message("implemented"),
            // verify：一次失败的测试执行，必须重新编号为 seq 3。
            tool_call_message("bash", json!({"command": "cargo test"})),
            tool_call_message("yield", json!({"review_findings": []})),
            text_message("verified"),
        ]);

        let work = harness
            .session
            .spawn_subagent_with_work(
                definition,
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    acceptance_criteria: vec!["tests_passed:cargo test".to_string()],
                    ..Default::default()
                },
                "ship and test",
                None,
                WorkOptions {
                    work_id: Some(WorkId(21)),
                    run_id: Some(RunId::new("run-21")),
                    store: Some(store),
                    ..Default::default()
                },
            )
            .await
            .expect("staged work");

        assert_eq!(work.result.terminal.status(), "success");
        // 如果不跨 stage 重新编号，verify 的 seq 会回落到 1，Work 级验收会选中
        // implement 的最后一次（通过）执行而误判为 Passed。
        let acceptance = work.result.acceptance.as_ref().expect("acceptance");
        assert_eq!(
            acceptance.report.status,
            AcceptanceStatus::Failed,
            "{acceptance:?}"
        );
        // evidence 与 execution 是两套视图：receipt.seq 同样必须跨 stage 单调，
        // 不能出现 [1, 1, 2] 之类的重复序号。
        let evidence_seq = work
            .result
            .evidence
            .iter()
            .map(|evidence| evidence.seq)
            .collect::<Vec<_>>();
        assert_eq!(evidence_seq, vec![1, 2, 3]);

        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn a_review_is_persisted_even_when_its_stage_fails() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state.path().join("continuity"))),
            ..Default::default()
        })
        .await;
        let session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");
        let findings = json!([{"severity": "warning", "summary": "partial review"}]);
        let mut definition = general_purpose();
        definition.stages = Some(vec![stage("verify", &[], &[StageArtifact::ReviewFindings])]);
        // 先 yield，再让 faux 队列耗尽：stage 自身以 Failed 收尾，但 findings 已产出。
        harness.set_responses(vec![tool_call_message(
            "yield",
            json!({"review_findings": findings.clone()}),
        )]);

        let work = harness
            .session
            .spawn_subagent_with_work(
                definition,
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "verify the change",
                None,
                WorkOptions {
                    work_id: Some(WorkId(22)),
                    run_id: Some(RunId::new("run-22")),
                    store: Some(store),
                    ..Default::default()
                },
            )
            .await
            .expect("work returns a terminal result");

        assert_eq!(work.result.terminal.status(), "failed");
        let path = review_path(
            harness.temp_dir.path(),
            &session_id,
            WorkId(22),
            &RunId::new("run-22"),
            "verify",
        );
        assert!(
            path.exists(),
            "review.json must be persisted before the stage's own failure: {}",
            path.display()
        );
        assert_eq!(
            load_review(
                &path,
                WorkId(22),
                &RunId::new("run-22"),
                &stage("verify", &[], &[StageArtifact::ReviewFindings]),
                0,
            )
            .expect("review round-trip"),
            findings
        );

        harness.cleanup();
    })
    .await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_kiana_directory_cannot_escape_the_workspace() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state.path().join("continuity"))),
            ..Default::default()
        })
        .await;
        // 克隆仓库可以把 `.kiana` 提交成指向工作区外的 symlink。
        let outside = state.path().join("outside");
        std::fs::create_dir_all(&outside).expect("outside");
        let kiana = harness.temp_dir.path().join(".kiana");
        if kiana.exists() {
            std::fs::remove_dir_all(&kiana).expect("clear .kiana");
        }
        std::os::unix::fs::symlink(&outside, &kiana).expect("symlink");

        let store = harness.session.durable_work_store().expect("work store");
        let mut definition = general_purpose();
        definition.stages = Some(vec![stage("verify", &[], &[StageArtifact::ReviewFindings])]);
        harness.set_responses(vec![
            tool_call_message("yield", json!({"review_findings": []})),
            text_message("verified"),
        ]);

        let work = harness
            .session
            .spawn_subagent_with_work(
                definition,
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "verify the change",
                None,
                WorkOptions {
                    work_id: Some(WorkId(23)),
                    run_id: Some(RunId::new("run-23")),
                    store: Some(store),
                    ..Default::default()
                },
            )
            .await
            .expect("work returns a terminal result");

        assert_eq!(work.result.terminal.status(), "failed");
        match &work.result.terminal {
            WorkTerminal::Failed { error, .. } => {
                assert!(error.contains("symlink"), "{error}");
            }
            other => panic!("expected failed terminal, got {other:?}"),
        }
        assert!(
            !outside.join("reviews").exists(),
            "review.json must not be written outside the workspace"
        );

        harness.cleanup();
    })
    .await;
}

/// 回归（round-16 finding #7）：程序化构造的定义不经过 `DefinitionBuilder::finish`，
/// 非法 stage 图必须在启动前按同一套规则拒绝，不能带进运行。
#[tokio::test]
async fn spawn_rejects_an_invalid_programmatic_stage_graph() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state.path().join("continuity"))),
            ..Default::default()
        })
        .await;

        let mut empty_produces = general_purpose();
        empty_produces.stages = Some(vec![stage("plan", &[], &[])]);
        let error = harness
            .session
            .spawn_subagent_with_work(
                empty_produces,
                SubagentRuntimeOptions::default(),
                "invalid graph",
                None,
                WorkOptions::default(),
            )
            .await
            .expect_err("a stage without produces must be rejected");
        assert!(
            error
                .to_string()
                .contains("must produce at least one artifact"),
            "{error}"
        );

        let mut consumes_before_produces = general_purpose();
        consumes_before_produces.stages = Some(vec![stage(
            "implement",
            &[StageArtifact::Plan],
            &[StageArtifact::ChangedFiles],
        )]);
        let error = harness
            .session
            .spawn_subagent_with_work(
                consumes_before_produces,
                SubagentRuntimeOptions::default(),
                "invalid graph",
                None,
                WorkOptions::default(),
            )
            .await
            .expect_err("consuming an unproduced artifact must be rejected");
        assert!(
            error
                .to_string()
                .contains("before any earlier stage produces it"),
            "{error}"
        );

        harness.cleanup();
    })
    .await;
}
