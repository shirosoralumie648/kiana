//! Phase 1a：durable Work 账本的端到端测试。
//!
//! 覆盖：
//! - 运行前 `begin`、准备后 `attach_session`、终态 `finish`；
//! - 新 store 实例（模拟进程重启）能读回终态记录；
//! - 同一 `workId` 的第二次调用在子代理运行前被拒绝；
//! - 崩溃遗留的 Running 记录被 `recover_work` 落定为 `interrupted`。

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
use kiana_session::agent_definition::builtin_agent_definitions;
use kiana_session::continuity::ContinuityOptions;
use kiana_session::subagent::{
    DurableWorkStore, RunId, SubagentRuntimeOptions, WorkAcceptancePolicy, WorkBegin, WorkId,
    WorkOptions, WorkRecord, WorkRecovery, WorkResult, WorkStore, WorkTerminal, build_work_request,
};
use serde_json::json;

struct CountingShellExecute {
    calls: Arc<AtomicUsize>,
}

impl AgentToolExecute for CountingShellExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: JsonValue,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(AgentToolResult {
                content: vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "test result: ok. 1 passed; 0 failed; 0 ignored".to_string(),
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

fn shell_tool(calls: Arc<AtomicUsize>) -> AgentTool {
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
        execute: Arc::new(CountingShellExecute { calls }),
    }
}

fn tool_call_message(name: &str, arguments: JsonValue) -> FauxResponseStep {
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

fn general_purpose() -> kiana_session::agent_definition::AgentDefinition {
    builtin_agent_definitions()
        .into_iter()
        .find(|definition| definition.name == "general-purpose")
        .expect("builtin general-purpose")
}

#[tokio::test]
async fn durable_work_records_terminal_and_survives_reopen() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let calls = Arc::new(AtomicUsize::new(0));
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![shell_tool(Arc::clone(&calls))]),
            continuity: Some(work_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            tool_call_message("bash", json!({"command": "cargo test"})),
            text_message("done"),
        ]);

        let store = harness.session.durable_work_store().expect("work store");
        let work_options = WorkOptions {
            work_id: Some(WorkId(41)),
            run_id: Some(RunId::new("run-41")),
            acceptance_policy: WorkAcceptancePolicy::RequirePassed,
            store: Some(store.clone()),
            ..Default::default()
        };
        let runtime_options = SubagentRuntimeOptions {
            session_dir: Some(state.path().join("subagent-sessions")),
            acceptance_criteria: vec!["tests_passed:cargo test".to_string()],
            ..Default::default()
        };

        let work = harness
            .session
            .spawn_subagent_with_work(
                general_purpose(),
                runtime_options,
                "run the tests",
                None,
                work_options,
            )
            .await
            .expect("work run");
        assert_eq!(work.result.terminal.status(), "success");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let parent_session_id = harness.session.session_id();
        let record = store
            .load(&parent_session_id, WorkId(41))
            .await
            .expect("load")
            .expect("record");
        assert!(record.is_terminal());
        assert_eq!(
            record
                .terminal_result()
                .expect("terminal result")
                .terminal
                .status(),
            "success"
        );
        assert!(
            record.session_file().is_some(),
            "persistent child session must be attached before the run"
        );

        // 新实例模拟进程重启：记录必须从 durable 文档读回。
        let reopened = DurableWorkStore::new(state_dir.join("work"));
        let reloaded = reopened
            .load(&parent_session_id, WorkId(41))
            .await
            .expect("load")
            .expect("record");
        assert!(reloaded.is_terminal());
        assert_eq!(
            reloaded
                .terminal_result()
                .expect("terminal result")
                .terminal
                .status(),
            "success"
        );

        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn duplicate_work_id_is_rejected_before_the_subagent_runs() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let calls = Arc::new(AtomicUsize::new(0));
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![shell_tool(Arc::clone(&calls))]),
            continuity: Some(work_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            tool_call_message("bash", json!({"command": "cargo test"})),
            text_message("done"),
        ]);

        let store = harness.session.durable_work_store().expect("work store");
        let first = WorkOptions {
            work_id: Some(WorkId(42)),
            run_id: Some(RunId::new("run-42")),
            store: Some(store.clone()),
            ..Default::default()
        };
        harness
            .session
            .spawn_subagent_with_work(
                general_purpose(),
                SubagentRuntimeOptions::default(),
                "first run",
                None,
                first,
            )
            .await
            .expect("first run");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // 同 workId、不同 runId：幂等键是 workId，必须在准备/运行前拒绝。
        let duplicate = WorkOptions {
            work_id: Some(WorkId(42)),
            run_id: Some(RunId::new("run-42b")),
            store: Some(store),
            ..Default::default()
        };
        let error = harness
            .session
            .spawn_subagent_with_work(
                general_purpose(),
                SubagentRuntimeOptions::default(),
                "second run",
                None,
                duplicate,
            )
            .await
            .expect_err("duplicate work id must fail");
        match &error {
            kiana_session::subagent::SubagentError::WorkAlreadyExists {
                work_id,
                status,
                run_id,
            } => {
                assert_eq!(*work_id, 42);
                assert_eq!(status, "terminal");
                assert_eq!(run_id, "run-42");
            }
            other => panic!("expected WorkAlreadyExists, got {other:?}"),
        }
        assert!(error.to_string().contains("already exists"), "{error}");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "duplicate must not execute the subagent again"
        );

        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn recover_work_terminalizes_orphaned_running_records() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let parent_session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");
        let request = build_work_request(
            &general_purpose(),
            &SubagentRuntimeOptions::default(),
            "orphaned work",
            state.path(),
            &WorkOptions {
                work_id: Some(WorkId(43)),
                run_id: Some(RunId::new("run-43")),
                ..Default::default()
            },
        );
        let record = WorkRecord::new(
            request,
            parent_session_id.clone(),
            None,
            None,
            1_700_000_000_000,
        );
        store.begin(record).await.expect("begin");

        let report = harness
            .session
            .recover_work()
            .await
            .expect("recover")
            .expect("report");
        assert_eq!(report.terminalized, vec![WorkId(43)]);

        let loaded = store
            .load(&parent_session_id, WorkId(43))
            .await
            .expect("load")
            .expect("record");
        assert!(loaded.is_terminal());
        let result = loaded.terminal_result().expect("terminal result");
        match &result.terminal {
            WorkTerminal::Failed { stop_reason, .. } => {
                assert_eq!(stop_reason.as_deref(), Some("interrupted"));
            }
            other => panic!("expected interrupted failure, got {other:?}"),
        }
        assert!(result.session_file.is_none());
        assert_eq!(loaded.terminal_seq(), 1);

        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn resume_policy_keeps_orphaned_work_for_explicit_recovery() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let parent_session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");
        let request = build_work_request(
            &general_purpose(),
            &SubagentRuntimeOptions::default(),
            "resumable work",
            state.path(),
            &WorkOptions {
                work_id: Some(WorkId(44)),
                run_id: Some(RunId::new("run-44")),
                recovery: WorkRecovery::Resume,
                ..Default::default()
            },
        );
        let record = WorkRecord::new(request, parent_session_id.clone(), None, None, 100);
        store.begin(record).await.expect("begin");
        // crash 发生在 attach_session 之后：记录里有 transcript，才能显式恢复。
        store
            .attach_session(
                &parent_session_id,
                WorkId(44),
                &RunId::new("run-44"),
                "/tmp/child.jsonl",
            )
            .await
            .expect("attach");

        let report = harness
            .session
            .recover_work()
            .await
            .expect("recover")
            .expect("report");
        assert_eq!(report.resumable, vec![WorkId(44)]);
        let loaded = store
            .load(&parent_session_id, WorkId(44))
            .await
            .expect("load")
            .expect("record");
        assert!(
            !loaded.is_terminal(),
            "resume policy must not auto-run or terminalize"
        );

        harness.cleanup();
    })
    .await;
}

/// `resume` 策略但没有 `sessionFile` 的 Running 记录永远无法 resume（崩溃发生在
/// `attach_session` 之前），必须落定为 interrupted，而不是无限期留在可恢复列表里。
#[tokio::test]
async fn resume_policy_terminalizes_work_without_a_session_file() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let parent_session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");
        let request = build_work_request(
            &general_purpose(),
            &SubagentRuntimeOptions::default(),
            "unresumable work",
            state.path(),
            &WorkOptions {
                work_id: Some(WorkId(46)),
                run_id: Some(RunId::new("run-46")),
                recovery: WorkRecovery::Resume,
                ..Default::default()
            },
        );
        let record = WorkRecord::new(request, parent_session_id.clone(), None, None, 100);
        store.begin(record).await.expect("begin");

        let report = harness
            .session
            .recover_work()
            .await
            .expect("recover")
            .expect("report");
        assert_eq!(report.terminalized, vec![WorkId(46)]);
        assert!(report.resumable.is_empty());

        let loaded = store
            .load(&parent_session_id, WorkId(46))
            .await
            .expect("load")
            .expect("record");
        let result = loaded.terminal_result().expect("terminal result");
        match &result.terminal {
            WorkTerminal::Failed { stop_reason, .. } => {
                assert_eq!(stop_reason.as_deref(), Some("interrupted"));
            }
            other => panic!("expected interrupted failure, got {other:?}"),
        }

        harness.cleanup();
    })
    .await;
}

/// 账本里的 pid 不是存活证据：能读到账本就说明本进程持有该会话的目录租约，另一个
/// 进程并没有在写。外来 pid（这里是确实活着的 pid 1）只能来自崩溃遗留或 pid 复用，
/// 必须按中断落定，否则被复用的 pid 会让 Work 永远停在 Running。
#[cfg(unix)]
#[tokio::test]
async fn recover_work_terminalizes_a_record_with_a_foreign_pid() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let parent_session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");
        let request = build_work_request(
            &general_purpose(),
            &SubagentRuntimeOptions::default(),
            "orphaned work",
            state.path(),
            &WorkOptions {
                work_id: Some(WorkId(45)),
                run_id: Some(RunId::new("run-45")),
                ..Default::default()
            },
        );
        // pid=1 在 unix 上恒存在：如果实现还在用 `kill(pid, 0)` 判定存活，这条记录
        // 就会被错误地当成“另一个进程仍在跑”而跳过。
        let record = WorkRecord::new(
            request,
            parent_session_id.clone(),
            None,
            Some(1),
            1_700_000_000_000,
        );
        store.begin(record).await.expect("begin");

        let report = harness
            .session
            .recover_work()
            .await
            .expect("recover")
            .expect("report");
        assert_eq!(report.terminalized, vec![WorkId(45)]);
        assert!(
            report.skipped_live.is_empty(),
            "a foreign pid must not be treated as a live owner"
        );

        let loaded = store
            .load(&parent_session_id, WorkId(45))
            .await
            .expect("load")
            .expect("record");
        assert!(
            loaded.is_terminal(),
            "a record left by a foreign pid must be terminalized"
        );

        harness.cleanup();
    })
    .await;
}

/// 未注入 `WorkStore` 时，`spawn_subagent_with_work` 保持 Phase 0 语义：
/// 正常运行、不产生 store 错误，也不在连续性目录下落 durable Work 记录。
#[tokio::test]
async fn spawn_without_store_keeps_phase0_behavior() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        assert!(
            harness.session.durable_work_store().is_some(),
            "continuity.work=true must create the session work store"
        );
        harness.set_responses(vec![text_message("done")]);

        let work = harness
            .session
            .spawn_subagent_with_work(
                general_purpose(),
                SubagentRuntimeOptions::default(),
                "plain run",
                None,
                WorkOptions::default(),
            )
            .await
            .expect("work run");
        assert_eq!(work.result.terminal.status(), "success");
        assert_eq!(work.store_error, None);
        assert!(
            !state_dir.join("work").exists(),
            "without an injected store no durable work record may be written"
        );

        harness.cleanup();
    })
    .await;
}

/// Phase 1b：崩溃遗留的 Running 记录在宿主显式 `resume_work` 后复用原 transcript 继续，
/// 原记录以 CAS 落定为 `interrupted`，新记录带 `resumedFrom` 与递增的 `attempts`。
#[tokio::test]
async fn resume_work_continues_an_interrupted_transcript() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let parent_session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");

        // 先真实跑一次，得到一个可恢复的 transcript。
        harness.set_responses(vec![text_message("first")]);
        let first = harness
            .session
            .spawn_subagent_with_work(
                general_purpose(),
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "start",
                None,
                WorkOptions {
                    work_id: Some(WorkId(47)),
                    run_id: Some(RunId::new("run-47")),
                    recovery: WorkRecovery::Resume,
                    store: Some(store.clone()),
                    ..Default::default()
                },
            )
            .await
            .expect("first run");
        let session_file = first
            .outcome
            .session_file
            .clone()
            .expect("persistent child session");
        let first_messages = first.outcome.messages.len();

        // 模拟“崩溃发生在 attach_session 之后”：账本里留下一条没有 pid 的 Running 记录。
        let request = build_work_request(
            &general_purpose(),
            &SubagentRuntimeOptions::default(),
            "crashed work",
            state.path(),
            &WorkOptions {
                work_id: Some(WorkId(48)),
                run_id: Some(RunId::new("run-48")),
                recovery: WorkRecovery::Resume,
                ..Default::default()
            },
        );
        store
            .begin(WorkRecord::new(
                request,
                parent_session_id.clone(),
                None,
                None,
                100,
            ))
            .await
            .expect("begin");
        store
            .attach_session(
                &parent_session_id,
                WorkId(48),
                &RunId::new("run-48"),
                &session_file,
            )
            .await
            .expect("attach");

        harness.set_responses(vec![text_message("second")]);
        let resumed = harness
            .session
            .resume_work(WorkId(48), "continue please", None)
            .await
            .expect("resume");

        assert_eq!(resumed.result.terminal.status(), "success");
        assert_eq!(resumed.request.prompt, "continue please");
        assert_ne!(resumed.request.work_id, WorkId(48));
        assert!(
            resumed.outcome.messages.len() > first_messages,
            "resume must append to the existing transcript instead of starting over"
        );

        // 原记录闭合为 interrupted，保留 transcript 供审计。
        let original = store
            .load(&parent_session_id, WorkId(48))
            .await
            .expect("load")
            .expect("original");
        assert!(original.is_terminal());
        match &original.terminal_result().expect("terminal").terminal {
            WorkTerminal::Failed { stop_reason, .. } => {
                assert_eq!(stop_reason.as_deref(), Some("interrupted"));
            }
            other => panic!("expected interrupted failure, got {other:?}"),
        }
        assert_eq!(original.session_file(), Some(session_file.as_str()));

        let resumed_record = store
            .load(&parent_session_id, resumed.request.work_id)
            .await
            .expect("load")
            .expect("resumed record");
        assert_eq!(resumed_record.resumed_from, Some(WorkId(48)));
        assert_eq!(resumed_record.attempts, 2);

        // 同一个锚点只能成功恢复一次：第二次必须被账本拒绝，不能再建一条记录。
        let error = harness
            .session
            .resume_work(WorkId(48), "again", None)
            .await
            .expect_err("an anchor can only be resumed once");
        assert!(
            error.to_string().contains("already been resumed"),
            "{error}"
        );
        assert_eq!(
            resumed_record
                .terminal_result()
                .expect("terminal")
                .terminal
                .status(),
            "success"
        );

        harness.cleanup();
    })
    .await;
}

/// 只有 `interrupted` 终态可以恢复；正常完成的 Work 不能被“重跑”。
#[tokio::test]
async fn resume_work_rejects_a_successful_record() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let store = harness.session.durable_work_store().expect("work store");
        harness.set_responses(vec![text_message("done")]);
        harness
            .session
            .spawn_subagent_with_work(
                general_purpose(),
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "run once",
                None,
                WorkOptions {
                    work_id: Some(WorkId(49)),
                    run_id: Some(RunId::new("run-49")),
                    recovery: WorkRecovery::Resume,
                    store: Some(store.clone()),
                    ..Default::default()
                },
            )
            .await
            .expect("work run");

        let error = harness
            .session
            .resume_work(WorkId(49), "again", None)
            .await
            .expect_err("a successful work must not be resumed");
        assert!(error.to_string().contains("not interrupted"), "{error}");

        harness.cleanup();
    })
    .await;
}

/// `recovery=fail` 的记录 `resumable == false`，即使被中断也不能恢复。
#[tokio::test]
async fn resume_work_rejects_a_non_resumable_record() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let parent_session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");
        let request = build_work_request(
            &general_purpose(),
            &SubagentRuntimeOptions::default(),
            "not resumable",
            state.path(),
            &WorkOptions {
                work_id: Some(WorkId(50)),
                run_id: Some(RunId::new("run-50")),
                ..Default::default()
            },
        );
        store
            .begin(WorkRecord::new(
                request,
                parent_session_id.clone(),
                None,
                None,
                100,
            ))
            .await
            .expect("begin");

        let error = harness
            .session
            .resume_work(WorkId(50), "continue", None)
            .await
            .expect_err("a non-resumable work must be rejected");
        assert!(error.to_string().contains("is not resumable"), "{error}");

        let record = store
            .load(&parent_session_id, WorkId(50))
            .await
            .expect("load")
            .expect("record");
        assert!(
            !record.is_terminal(),
            "a rejected resume must not terminalize the original record"
        );

        harness.cleanup();
    })
    .await;
}

/// 没有 `sessionFile` 的记录无法恢复；拒绝时也不能顺手把它落定。
#[tokio::test]
async fn resume_work_requires_a_session_file() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let parent_session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");
        let request = build_work_request(
            &general_purpose(),
            &SubagentRuntimeOptions::default(),
            "no transcript",
            state.path(),
            &WorkOptions {
                work_id: Some(WorkId(51)),
                run_id: Some(RunId::new("run-51")),
                recovery: WorkRecovery::Resume,
                ..Default::default()
            },
        );
        store
            .begin(WorkRecord::new(
                request,
                parent_session_id.clone(),
                None,
                None,
                100,
            ))
            .await
            .expect("begin");

        let error = harness
            .session
            .resume_work(WorkId(51), "continue", None)
            .await
            .expect_err("work without a session file cannot be resumed");
        assert!(error.to_string().contains("no session file"), "{error}");

        let record = store
            .load(&parent_session_id, WorkId(51))
            .await
            .expect("load")
            .expect("record");
        assert!(!record.is_terminal());

        harness.cleanup();
    })
    .await;
}

/// 未知 workId 报错而不是隐式创建。
#[tokio::test]
async fn resume_work_rejects_an_unknown_work_id() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state_dir)),
            ..Default::default()
        })
        .await;

        let error = harness
            .session
            .resume_work(WorkId(9_999), "continue", None)
            .await
            .expect_err("unknown work id must fail");
        assert!(error.to_string().contains("does not exist"), "{error}");

        harness.cleanup();
    })
    .await;
}

/// 后继在打开 transcript 时失败（例如文件损坏），锚点必须还回去：宿主能重试同一个原记录。
#[tokio::test]
async fn a_failed_resume_releases_the_anchor() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let parent_session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");

        let corrupt = state.path().join("corrupt-child.jsonl");
        std::fs::write(&corrupt, "not a session\n").expect("corrupt transcript");
        let request = build_work_request(
            &general_purpose(),
            &SubagentRuntimeOptions::default(),
            "crashed work",
            state.path(),
            &WorkOptions {
                work_id: Some(WorkId(52)),
                run_id: Some(RunId::new("run-52")),
                recovery: WorkRecovery::Resume,
                ..Default::default()
            },
        );
        store
            .begin(WorkRecord::new(
                request.clone(),
                parent_session_id.clone(),
                None,
                None,
                100,
            ))
            .await
            .expect("begin");
        store
            .attach_session(
                &parent_session_id,
                WorkId(52),
                &RunId::new("run-52"),
                &corrupt.to_string_lossy(),
            )
            .await
            .expect("attach");
        store
            .finish(
                &parent_session_id,
                &WorkResult::interrupted(
                    &request,
                    "work interrupted",
                    Some(corrupt.to_string_lossy().into_owned()),
                    100,
                    200,
                ),
            )
            .await
            .expect("finish");

        harness
            .session
            .resume_work(WorkId(52), "continue", None)
            .await
            .expect_err("a corrupt transcript cannot be resumed");

        // 失败的后继已经落定，但不再占用锚点；新的后继可以 begin。
        let records = store
            .list_for_session(&parent_session_id)
            .await
            .expect("list");
        let successor = records
            .iter()
            .find(|record| record.request.prompt == "continue")
            .expect("failed successor record");
        assert!(successor.is_terminal());
        assert_eq!(successor.resumed_from, None);

        let mut retry_request = request.clone();
        retry_request.work_id = store
            .allocate_work_id(&parent_session_id)
            .await
            .expect("allocate");
        retry_request.run_id = RunId::new("run-retry");
        let retry = WorkRecord::new(retry_request, parent_session_id.clone(), None, None, 300)
            .with_resumed_from(WorkId(52));
        assert!(matches!(
            store.begin(retry).await.expect("retry begin"),
            WorkBegin::Started(_)
        ));

        harness.cleanup();
    })
    .await;
}

/// 记录里的 transcript 还没落盘（首条消息前崩溃）：恢复按新会话继续，而不是失败。
#[tokio::test]
async fn resume_work_starts_a_fresh_session_when_the_file_is_missing() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let parent_session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");

        // `attach_session` 在 `prepare` 之后立刻写路径，但文件要等首条消息才创建。
        let missing = state
            .path()
            .join("subagent-sessions")
            .join("missing-child.jsonl");
        assert!(!missing.exists());
        let request = build_work_request(
            &general_purpose(),
            &SubagentRuntimeOptions::default(),
            "crashed work",
            state.path(),
            &WorkOptions {
                work_id: Some(WorkId(53)),
                run_id: Some(RunId::new("run-53")),
                recovery: WorkRecovery::Resume,
                ..Default::default()
            },
        );
        store
            .begin(WorkRecord::new(
                request.clone(),
                parent_session_id.clone(),
                None,
                None,
                100,
            ))
            .await
            .expect("begin");
        store
            .attach_session(
                &parent_session_id,
                WorkId(53),
                &RunId::new("run-53"),
                &missing.to_string_lossy(),
            )
            .await
            .expect("attach");
        store
            .finish(
                &parent_session_id,
                &WorkResult::interrupted(
                    &request,
                    "work interrupted",
                    Some(missing.to_string_lossy().into_owned()),
                    100,
                    200,
                ),
            )
            .await
            .expect("finish");

        harness.set_responses(vec![text_message("recovered")]);
        let resumed = harness
            .session
            .resume_work(WorkId(53), "continue", None)
            .await
            .expect("resume must not fail on a missing transcript");
        assert_eq!(resumed.result.terminal.status(), "success");

        harness.cleanup();
    })
    .await;
}

/// 崩溃遗留的恢复后继被 `recover_work` 落定后，必须归还锚点：宿主能重试原记录。
#[tokio::test]
async fn recover_work_releases_the_anchor_of_a_crashed_successor() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let parent_session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");

        // 先真实跑一次，得到一个可恢复的 transcript。
        harness.set_responses(vec![text_message("first")]);
        let first = harness
            .session
            .spawn_subagent_with_work(
                general_purpose(),
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "start",
                None,
                WorkOptions {
                    work_id: Some(WorkId(54)),
                    run_id: Some(RunId::new("run-54")),
                    recovery: WorkRecovery::Resume,
                    store: Some(store.clone()),
                    ..Default::default()
                },
            )
            .await
            .expect("first run");
        let session_file = first
            .outcome
            .session_file
            .clone()
            .expect("persistent child session");

        // 模拟崩溃遗留的 Running 锚点 W54。
        let anchor_request = build_work_request(
            &general_purpose(),
            &SubagentRuntimeOptions::default(),
            "crashed work",
            state.path(),
            &WorkOptions {
                work_id: Some(WorkId(55)),
                run_id: Some(RunId::new("run-55")),
                recovery: WorkRecovery::Resume,
                ..Default::default()
            },
        );
        store
            .begin(WorkRecord::new(
                anchor_request,
                parent_session_id.clone(),
                None,
                None,
                100,
            ))
            .await
            .expect("anchor begin");
        store
            .attach_session(
                &parent_session_id,
                WorkId(55),
                &RunId::new("run-55"),
                &session_file,
            )
            .await
            .expect("anchor attach");

        // 模拟崩溃遗留的后继：它已占用锚点，但进程在运行期间消失。
        let successor_request = build_work_request(
            &general_purpose(),
            &SubagentRuntimeOptions::default(),
            "crashed successor",
            state.path(),
            &WorkOptions {
                work_id: Some(WorkId(56)),
                run_id: Some(RunId::new("run-56")),
                ..Default::default()
            },
        );
        store
            .begin(
                WorkRecord::new(
                    successor_request,
                    parent_session_id.clone(),
                    None,
                    None,
                    200,
                )
                .with_resumed_from(WorkId(55)),
            )
            .await
            .expect("successor begin");

        let report = harness
            .session
            .recover_work()
            .await
            .expect("recover")
            .expect("report");
        assert_eq!(report.terminalized, vec![WorkId(56)]);
        assert_eq!(report.resumable, vec![WorkId(55)]);
        let successor = store
            .load(&parent_session_id, WorkId(56))
            .await
            .expect("load successor")
            .expect("successor record");
        assert!(successor.is_terminal());
        assert_eq!(
            successor.resumed_from, None,
            "recovering a crashed successor must release the anchor"
        );

        // 修复前，终态后继仍占着锚点，这次恢复会被 AlreadyResumed 拒绝。
        harness.set_responses(vec![text_message("after recovery")]);
        let resumed = harness
            .session
            .resume_work(WorkId(55), "continue after crash", None)
            .await
            .expect("the anchor must be reusable after recovery");
        assert_eq!(resumed.result.terminal.status(), "success");
        assert_eq!(resumed.request.work_id, WorkId(57));

        harness.cleanup();
    })
    .await;
}

/// “finish 已提交、release_resume 尚未提交”之间崩溃：启动恢复必须补做释放。
#[tokio::test]
async fn recover_work_repairs_a_terminal_successor_that_still_holds_the_anchor() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let parent_session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");

        harness.set_responses(vec![text_message("first")]);
        let first = harness
            .session
            .spawn_subagent_with_work(
                general_purpose(),
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "start",
                None,
                WorkOptions {
                    work_id: Some(WorkId(60)),
                    run_id: Some(RunId::new("run-60")),
                    recovery: WorkRecovery::Resume,
                    store: Some(store.clone()),
                    ..Default::default()
                },
            )
            .await
            .expect("first run");
        let session_file = first
            .outcome
            .session_file
            .clone()
            .expect("persistent child session");

        let anchor_request = build_work_request(
            &general_purpose(),
            &SubagentRuntimeOptions::default(),
            "crashed work",
            state.path(),
            &WorkOptions {
                work_id: Some(WorkId(61)),
                run_id: Some(RunId::new("run-61")),
                recovery: WorkRecovery::Resume,
                ..Default::default()
            },
        );
        store
            .begin(WorkRecord::new(
                anchor_request,
                parent_session_id.clone(),
                None,
                None,
                100,
            ))
            .await
            .expect("anchor begin");
        store
            .attach_session(
                &parent_session_id,
                WorkId(61),
                &RunId::new("run-61"),
                &session_file,
            )
            .await
            .expect("anchor attach");

        let successor_request = build_work_request(
            &general_purpose(),
            &SubagentRuntimeOptions::default(),
            "crashed successor",
            state.path(),
            &WorkOptions {
                work_id: Some(WorkId(62)),
                run_id: Some(RunId::new("run-62")),
                ..Default::default()
            },
        );
        store
            .begin(
                WorkRecord::new(
                    successor_request.clone(),
                    parent_session_id.clone(),
                    None,
                    None,
                    200,
                )
                .with_resumed_from(WorkId(61)),
            )
            .await
            .expect("successor begin");
        // 模拟崩溃窗口：终态已提交，但 release_resume 没有发生。
        store
            .finish(
                &parent_session_id,
                &WorkResult::interrupted(
                    &successor_request,
                    "work interrupted: process lost",
                    Some(session_file.clone()),
                    200,
                    300,
                ),
            )
            .await
            .expect("finish successor");
        let successor = store
            .load(&parent_session_id, WorkId(62))
            .await
            .expect("load successor")
            .expect("successor record");
        assert_eq!(successor.resumed_from, Some(WorkId(61)));

        let report = harness
            .session
            .recover_work()
            .await
            .expect("recover")
            .expect("report");
        assert_eq!(report.resumable, vec![WorkId(61)]);
        let repaired = store
            .load(&parent_session_id, WorkId(62))
            .await
            .expect("load repaired")
            .expect("successor record");
        assert_eq!(
            repaired.resumed_from, None,
            "recover_work must repair a terminal successor that still holds the anchor"
        );

        harness.set_responses(vec![text_message("after repair")]);
        let resumed = harness
            .session
            .resume_work(WorkId(61), "continue after repair", None)
            .await
            .expect("the repaired anchor must be resumable");
        assert_eq!(resumed.result.terminal.status(), "success");
        assert_eq!(resumed.request.work_id, WorkId(63));

        harness.cleanup();
    })
    .await;
}

/// 回归（round-16 finding #6）：`recovery=resume` 且未显式注入 store 时，spawn 必须
/// 回退到会话的 durable 账本。否则 work id 来自进程内计数器，与 `resume_work`
/// 读取的 per-session 账本不是同一命名空间，恢复可能命中另一个 Work。
#[tokio::test]
async fn resumable_spawn_without_an_injected_store_uses_the_session_ledger() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let store = harness.session.durable_work_store().expect("work store");
        harness.set_responses(vec![text_message("done")]);

        let work = harness
            .session
            .spawn_subagent_with_work(
                general_purpose(),
                SubagentRuntimeOptions {
                    session_dir: Some(state.path().join("subagent-sessions")),
                    ..Default::default()
                },
                "resumable without an injected store",
                None,
                WorkOptions {
                    recovery: WorkRecovery::Resume,
                    ..Default::default()
                },
            )
            .await
            .expect("work run");

        let record = store
            .load(&harness.session.session_id(), work.request.work_id)
            .await
            .expect("load")
            .unwrap_or_else(|| {
                panic!(
                    "resumable work {} must be recorded in the session ledger",
                    work.request.work_id.0
                )
            });
        assert_eq!(record.run_id, work.request.run_id);
        assert!(record.request.resumable);
        assert!(record.is_terminal());

        harness.cleanup();
    })
    .await;
}

/// 回归（round-16 finding #3）：归还锚点只能看一层。W1→W2(interrupted)→W3(Running)
/// 时 W2 仍被 W3 引用；`recover_work` 只能归还 W3 的锚点，不能把 W1 也放出去，
/// 否则宿主恢复 W1 会与 W3 并发写同一 transcript/workspace。
#[tokio::test]
async fn recover_work_does_not_release_an_anchor_that_still_has_a_successor() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let parent_session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");
        let transcript = state
            .path()
            .join("crashed-child.jsonl")
            .to_string_lossy()
            .into_owned();
        let request = |work_id: i64, recovery: WorkRecovery| {
            build_work_request(
                &general_purpose(),
                &SubagentRuntimeOptions::default(),
                "crashed work",
                state.path(),
                &WorkOptions {
                    work_id: Some(WorkId(work_id)),
                    run_id: Some(RunId::new(format!("run-{work_id}"))),
                    recovery,
                    ..Default::default()
                },
            )
        };

        // W70：可恢复的 interrupted 原记录。
        let w70 = request(70, WorkRecovery::Resume);
        store
            .begin(WorkRecord::new(
                w70.clone(),
                parent_session_id.clone(),
                None,
                None,
                100,
            ))
            .await
            .expect("begin 70");
        store
            .finish(
                &parent_session_id,
                &WorkResult::interrupted(
                    &w70,
                    "work interrupted: process lost",
                    Some(transcript.clone()),
                    100,
                    200,
                ),
            )
            .await
            .expect("finish 70");

        // W71：W70 的后继，已经 interrupted。
        let w71 = request(71, WorkRecovery::Resume);
        store
            .begin(
                WorkRecord::new(w71.clone(), parent_session_id.clone(), None, None, 300)
                    .with_resumed_from(WorkId(70)),
            )
            .await
            .expect("begin 71");
        store
            .finish(
                &parent_session_id,
                &WorkResult::interrupted(
                    &w71,
                    "work interrupted: process lost",
                    Some(transcript.clone()),
                    300,
                    400,
                ),
            )
            .await
            .expect("finish 71");

        // W72：W71 的后继，崩溃遗留的 Running（同一条链上的第二个执行者）。
        let w72 = request(72, WorkRecovery::Fail);
        store
            .begin(
                WorkRecord::new(w72, parent_session_id.clone(), None, None, 500)
                    .with_resumed_from(WorkId(71)),
            )
            .await
            .expect("begin 72");

        let report = harness
            .session
            .recover_work()
            .await
            .expect("recover")
            .expect("report");
        assert_eq!(report.terminalized, vec![WorkId(72)]);

        let w71_after = store
            .load(&parent_session_id, WorkId(71))
            .await
            .expect("load 71")
            .expect("71");
        assert_eq!(
            w71_after.resumed_from,
            Some(WorkId(70)),
            "an interrupted successor that itself has a successor must keep its anchor"
        );
        let w72_after = store
            .load(&parent_session_id, WorkId(72))
            .await
            .expect("load 72")
            .expect("72");
        assert_eq!(
            w72_after.resumed_from, None,
            "recovering W72 must still release W71's anchor"
        );

        let error = harness
            .session
            .resume_work(WorkId(70), "resume", None)
            .await
            .expect_err("W70 stays anchored while W71 exists");
        assert!(
            error.to_string().contains("already been resumed"),
            "{error}"
        );

        harness.cleanup();
    })
    .await;
}

/// round-18 finding #1/#2：只有 lineage 的 tip 可恢复。整条链都是可恢复的
/// interrupted 记录时，锚点必须保持占用——释放会让原记录与后继同时出现在可恢复
/// 集合里，两个 runtime 追加同一个 child JSONL。
#[tokio::test]
async fn recover_work_keeps_a_resumable_chain_anchored_to_its_tip() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let parent_session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");
        let transcript = state
            .path()
            .join("crashed-child.jsonl")
            .to_string_lossy()
            .into_owned();
        let request = |work_id: i64| {
            build_work_request(
                &general_purpose(),
                &SubagentRuntimeOptions::default(),
                "crashed work",
                state.path(),
                &WorkOptions {
                    work_id: Some(WorkId(work_id)),
                    run_id: Some(RunId::new(format!("run-{work_id}"))),
                    recovery: WorkRecovery::Resume,
                    ..Default::default()
                },
            )
        };

        for (work_id, resumed_from, created_at) in [
            (80_i64, None, 100_u64),
            (81_i64, Some(WorkId(80)), 200),
            (82_i64, Some(WorkId(81)), 300),
        ] {
            let request = request(work_id);
            let mut record = WorkRecord::new(
                request.clone(),
                parent_session_id.clone(),
                None,
                None,
                created_at,
            );
            if let Some(origin) = resumed_from {
                record = record.with_resumed_from(origin);
            }
            store.begin(record).await.expect("begin");
            store
                .finish(
                    &parent_session_id,
                    &WorkResult::interrupted(
                        &request,
                        "work interrupted: process lost",
                        Some(transcript.clone()),
                        created_at,
                        created_at + 10,
                    ),
                )
                .await
                .expect("finish");
        }

        harness.session.recover_work().await.expect("recover");

        for (work_id, origin) in [(81, 80), (82, 81)] {
            let record = store
                .load(&parent_session_id, WorkId(work_id))
                .await
                .expect("load")
                .expect("record");
            assert_eq!(
                record.resumed_from,
                Some(WorkId(origin)),
                "the resumable tip {work_id} keeps the lineage anchored"
            );
        }
        assert!(matches!(
            store
                .begin(
                    WorkRecord::new(request(83), parent_session_id.clone(), None, None, 400)
                        .with_resumed_from(WorkId(80)),
                )
                .await
                .expect("blocked"),
            WorkBegin::AlreadyResumed(_)
        ));
        // tip W82 才是宿主应恢复的记录。
        assert!(matches!(
            store
                .begin(
                    WorkRecord::new(request(84), parent_session_id.clone(), None, None, 500)
                        .with_resumed_from(WorkId(82)),
                )
                .await
                .expect("tip"),
            WorkBegin::Started(_)
        ));

        harness.cleanup();
    })
    .await;
}

/// round-18 finding #5：落定 tip 与归还锚点必须在同一轮交替到 fixpoint。非可恢复的
/// 死链（recovery=fail，resume_work 不接受）应在一次 `recover_work` 里全部松开，
/// 原记录立刻可以再次被恢复。
#[tokio::test]
async fn recover_work_unwinds_a_non_resumable_dead_chain_in_one_pass() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let parent_session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");
        let transcript = state
            .path()
            .join("crashed-child.jsonl")
            .to_string_lossy()
            .into_owned();
        let request = |work_id: i64, recovery: WorkRecovery| {
            build_work_request(
                &general_purpose(),
                &SubagentRuntimeOptions::default(),
                "crashed work",
                state.path(),
                &WorkOptions {
                    work_id: Some(WorkId(work_id)),
                    run_id: Some(RunId::new(format!("run-{work_id}"))),
                    recovery,
                    ..Default::default()
                },
            )
        };

        for (work_id, recovery, resumed_from, created_at) in [
            (85_i64, WorkRecovery::Resume, None, 100_u64),
            (86_i64, WorkRecovery::Fail, Some(WorkId(85)), 200),
            (87_i64, WorkRecovery::Fail, Some(WorkId(86)), 300),
        ] {
            let request = request(work_id, recovery);
            let mut record = WorkRecord::new(
                request.clone(),
                parent_session_id.clone(),
                None,
                None,
                created_at,
            );
            if let Some(origin) = resumed_from {
                record = record.with_resumed_from(origin);
            }
            store.begin(record).await.expect("begin");
            store
                .finish(
                    &parent_session_id,
                    &WorkResult::interrupted(
                        &request,
                        "work interrupted: process lost",
                        Some(transcript.clone()),
                        created_at,
                        created_at + 10,
                    ),
                )
                .await
                .expect("finish");
        }

        harness.session.recover_work().await.expect("recover");

        for work_id in [86, 87] {
            let record = store
                .load(&parent_session_id, WorkId(work_id))
                .await
                .expect("load")
                .expect("record");
            assert_eq!(
                record.resumed_from, None,
                "non-resumable dead successor {work_id} must release its anchor"
            );
        }
        assert!(matches!(
            store
                .begin(
                    WorkRecord::new(
                        request(88, WorkRecovery::Resume),
                        parent_session_id.clone(),
                        None,
                        None,
                        400,
                    )
                    .with_resumed_from(WorkId(85)),
                )
                .await
                .expect("W85 must be resumable again"),
            WorkBegin::Started(_)
        ));

        harness.cleanup();
    })
    .await;
}

/// round-18 finding #1：非 Success 且不能作为恢复目标的终态（例如 Cancelled）
/// 必须归还锚点，否则原记录永久不可重试。Cancelled 记录即使声明了 resumable，
/// `resume_work` 也只接受 interrupted，因此它不是可行的 tip。
#[tokio::test]
async fn recover_work_releases_the_anchor_of_a_cancelled_successor() {
    run(async {
        let state = tempfile::tempdir().expect("state");
        let state_dir = state.path().join("continuity");
        let harness = create_harness(HarnessOptions {
            continuity: Some(work_continuity(&state_dir)),
            ..Default::default()
        })
        .await;
        let parent_session_id = harness.session.session_id();
        let store = harness.session.durable_work_store().expect("work store");
        let transcript = state
            .path()
            .join("crashed-child.jsonl")
            .to_string_lossy()
            .into_owned();
        let request = |work_id: i64| {
            build_work_request(
                &general_purpose(),
                &SubagentRuntimeOptions::default(),
                "crashed work",
                state.path(),
                &WorkOptions {
                    work_id: Some(WorkId(work_id)),
                    run_id: Some(RunId::new(format!("run-{work_id}"))),
                    recovery: WorkRecovery::Resume,
                    ..Default::default()
                },
            )
        };

        let anchor = request(90);
        store
            .begin(WorkRecord::new(
                anchor.clone(),
                parent_session_id.clone(),
                None,
                None,
                100,
            ))
            .await
            .expect("anchor begin");
        store
            .finish(
                &parent_session_id,
                &WorkResult::interrupted(
                    &anchor,
                    "work interrupted: process lost",
                    Some(transcript.clone()),
                    100,
                    200,
                ),
            )
            .await
            .expect("anchor finish");

        let successor = request(91);
        store
            .begin(
                WorkRecord::new(
                    successor.clone(),
                    parent_session_id.clone(),
                    None,
                    None,
                    300,
                )
                .with_resumed_from(WorkId(90)),
            )
            .await
            .expect("successor begin");
        let mut cancelled = WorkResult::failure(&successor, "cancelled", 300, 400);
        cancelled.terminal = WorkTerminal::Cancelled {
            reason: Some("host cancelled".to_string()),
        };
        cancelled.session_file = Some(transcript);
        store
            .finish(&parent_session_id, &cancelled)
            .await
            .expect("successor finish");

        harness.session.recover_work().await.expect("recover");

        let repaired = store
            .load(&parent_session_id, WorkId(91))
            .await
            .expect("load")
            .expect("record");
        assert_eq!(
            repaired.resumed_from, None,
            "a cancelled successor cannot be resumed; the anchor must be returned"
        );
        assert!(matches!(
            store
                .begin(
                    WorkRecord::new(request(92), parent_session_id.clone(), None, None, 500,)
                        .with_resumed_from(WorkId(90)),
                )
                .await
                .expect("the original must be resumable again"),
            WorkBegin::Started(_)
        ));

        harness.cleanup();
    })
    .await;
}
