//! 第 11 周后半接线：恢复分类/修复、continuity（checkpoint/ledger/验收）的会话级测试。

mod common;

use std::sync::Arc;

use common::*;
use kiana_agent_core::types::{AgentTool, AgentToolError, AgentToolResult, BoxFuture};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Message, StopReason, TextContent,
    TextKind, ToolCall, ToolCallKind, Usage, UsageCost, UserContent, UserContentBlock, UserMessage,
    UserRole,
};
use kiana_session::continuity::ContinuityOptions;
use kiana_session_core::recovery::{RecoveryClass, RecoveryContinuation};
use kiana_session_core::session_manager::SessionManager;
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn usage() -> Usage {
    Usage {
        input: 1,
        output: 1,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: None,
        reasoning: None,
        total_tokens: 2,
        cost: UsageCost::default(),
    }
}

fn user_message(text: &str, timestamp: i64) -> kiana_agent_core::AgentMessage {
    kiana_agent_core::AgentMessage::Standard(Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.to_string()),
        timestamp,
    }))
}

fn tool_call(id: &str, name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        kind_tool_call: ToolCallKind::ToolCall,
        id: id.to_string(),
        name: name.to_string(),
        arguments: arguments
            .as_object()
            .map(|object| {
                object
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default(),
        thought_signature: None,
        namespace: None,
        arguments_error: None,
    }
}

fn assistant_with_call(id: &str, name: &str, timestamp: i64) -> kiana_agent_core::AgentMessage {
    let arguments = json!({"path": "file.txt", "content": "hello"});
    kiana_agent_core::AgentMessage::Standard(Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::ToolCall(tool_call(
            id, name, arguments,
        ))],
        api: "anthropic-messages".to_string(),
        provider: "anthropic".to_string(),
        model: "test".to_string(),
        usage: usage(),
        stop_reason: StopReason::ToolUse,
        timestamp,
        ..Default::default()
    }))
}

fn manager_with(messages: Vec<kiana_agent_core::AgentMessage>) -> SessionManager {
    let mut manager = SessionManager::in_memory("/tmp/kiana-continuity-wiring", None, None)
        .expect("in-memory session");
    for message in messages {
        manager.append_message(message).expect("append");
    }
    manager
}

struct WriteProbe;

impl kiana_agent_core::types::AgentToolExecute for WriteProbe {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: serde_json::Value,
        _signal: Option<&'a CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
    ) -> BoxFuture<'a, Result<AgentToolResult<serde_json::Value>, AgentToolError>> {
        Box::pin(async move {
            let path = params
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| AgentToolError {
                    message: "missing path".to_string(),
                    name: None,
                    stack: None,
                })?;
            let content = params
                .get("content")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            std::fs::write(path, content).map_err(|error| AgentToolError {
                message: error.to_string(),
                name: None,
                stack: None,
            })?;
            Ok(AgentToolResult {
                content: vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: format!("wrote {path}"),
                    text_signature: None,
                })],
                details: serde_json::Value::Null,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

fn write_probe_tool() -> AgentTool {
    AgentTool {
        name: "write".to_string(),
        label: "Write".to_string(),
        description: "Write a file".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"}
            },
            "required": ["path"]
        }),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(WriteProbe),
    }
}

/// InterruptedTurn：会话构造时分类、写 canonical 合成结果，并让 Agent 历史看到它。
#[tokio::test]
async fn interrupted_turn_is_repaired_before_agent_history() {
    run(async {
        let manager = manager_with(vec![
            user_message("write the file", 1),
            assistant_with_call("call-1", "write", 2),
        ]);
        let harness = create_harness(HarnessOptions {
            session_manager: Some(manager),
            ..Default::default()
        })
        .await;

        assert_eq!(
            harness.session.recovery_classification(),
            Some(RecoveryClass::InterruptedTurn)
        );
        let tool_results: Vec<_> = harness
            .session
            .messages()
            .into_iter()
            .filter(|message| message.role() == "toolResult")
            .collect();
        assert_eq!(tool_results.len(), 1, "one synthetic result");
        let branch = harness.session.session_manager().get_branch(None);
        let canonical_results: Vec<_> = branch
            .iter()
            .filter_map(|entry| match entry {
                kiana_session_core::session_manager::SessionEntry::Message(message) => {
                    (message.message.role() == "toolResult").then_some(message)
                }
                _ => None,
            })
            .collect();
        assert_eq!(canonical_results.len(), 1, "canonical repair is persisted");
        let continuation = harness
            .session
            .take_recovery_continuation()
            .expect("continuation");
        assert!(matches!(
            continuation,
            RecoveryContinuation::SyntheticToolResults { .. }
        ));
        assert!(
            harness.session.take_recovery_continuation().is_none(),
            "continuation is delivered once"
        );
        harness.cleanup();
    })
    .await;
}

/// InterruptedPrompt：尾部 user 的 entry id 稳定、canonical 不新增 user entry。
#[tokio::test]
async fn interrupted_prompt_exposes_a_single_replay_and_keeps_canonical() {
    run(async {
        let manager = manager_with(vec![user_message("finish the task", 1)]);
        let before = manager.get_branch(None).len();
        let harness = create_harness(HarnessOptions {
            session_manager: Some(manager),
            ..Default::default()
        })
        .await;
        assert_eq!(
            harness.session.recovery_classification(),
            Some(RecoveryClass::InterruptedPrompt)
        );
        match harness.session.take_recovery_continuation() {
            Some(RecoveryContinuation::ReplayUser { entry_id, message }) => {
                assert!(!entry_id.is_empty());
                assert_eq!(message.role(), "user");
            }
            other => panic!("expected ReplayUser, got {other:?}"),
        }
        assert_eq!(
            harness.session.session_manager().get_branch(None).len(),
            before,
            "no new canonical user entry"
        );
        harness.cleanup();
    })
    .await;
}

/// continuity：mutating tool 前 checkpoint、成功后 ledger + acceptance 记录。
#[tokio::test]
async fn continuity_checkpoints_and_records_accepted_writes() {
    run(async {
        let state_dir = tempfile::tempdir().expect("state dir");
        let options = ContinuityOptions {
            state_dir: state_dir.path().to_path_buf(),
            execution: false, // 执行所有权有独立单测；这里聚焦 checkpoint/ledger/acceptance
            checkpoints: true,
            background: false,
            acceptance: true,
            ..ContinuityOptions::new(state_dir.path())
        };
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![write_probe_tool()]),
            continuity: Some(options),
            ..Default::default()
        })
        .await;
        let cwd = harness.temp_dir.path().to_path_buf();
        let target = cwd.join("file.txt");
        harness.set_responses(vec![
            faux_tool_response(
                tool_call(
                    "call-1",
                    "write",
                    json!({"path": target.to_string_lossy(), "content": "hello"}),
                ),
                StopReason::ToolUse,
            ),
            faux_text_response("done"),
        ]);
        harness
            .session
            .prompt("write it".to_string(), Default::default())
            .await
            .expect("prompt");

        assert_eq!(std::fs::read_to_string(&target).expect("file"), "hello");
        let cwd = cwd.as_path();
        let manager = harness
            .session
            .checkpoint_manager()
            .expect("checkpoint manager");
        let metadata = manager
            .latest_checkpoint(cwd)
            .expect("read metadata")
            .expect("checkpoint created before mutating tool");
        assert!(!metadata.commit.is_empty());
        let ledger = manager.ledger(cwd).expect("ledger");
        assert!(
            ledger.entries().contains_key("file.txt"),
            "agent write recorded: {:?}",
            ledger.entries().keys().collect::<Vec<_>>()
        );

        let records = harness.session.acceptance_records();
        assert!(
            records
                .iter()
                .any(|record| record.tool == "write" && record.verify()),
            "runtime-stamped execution record: {records:?}"
        );
        let report = harness.session.evaluate_acceptance(&[
            "file:file.txt exists".to_string(),
            "file:file.txt non-empty".to_string(),
        ]);
        assert!(report.all_hold(), "{report:?}");
        assert!(
            harness.session.continuity_diagnostics().is_empty()
                || harness
                    .session
                    .continuity_diagnostics()
                    .iter()
                    .all(|line| !line.contains("checkpoint failed")),
            "{:?}",
            harness.session.continuity_diagnostics()
        );
        // 用户手改后 restore 必须跳过并报告。
        std::fs::write(cwd.join("file.txt"), "user edit").expect("user edit");
        let restore = harness
            .session
            .restore_checkpoint()
            .expect("restore")
            .expect("report");
        assert!(
            restore
                .skipped_user_edits
                .iter()
                .any(|path| path.contains("file.txt")),
            "{restore:?}"
        );
        harness.cleanup();
    })
    .await;
}

/// 连续开关关闭时所有入口都是 no-op，不创建状态目录。
#[tokio::test]
async fn continuity_is_off_by_default() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        assert!(!harness.session.continuity_enabled());
        assert!(harness.session.acceptance_records().is_empty());
        assert!(harness.session.checkpoint_manager().is_none());
        let report = harness
            .session
            .evaluate_acceptance(&["file:anything exists".to_string()]);
        assert!(!report.all_hold(), "offline acceptance is unverified");
        harness.cleanup();
    })
    .await;
}

/// 执行 claim 的生命周期是 per-run：一次 prompt 结束（成功）后 claim 释放，
/// Terminal 事件恰好发布一次；下一次 prompt 重新 acquire。
#[tokio::test]
async fn execution_claim_is_released_and_terminal_is_published_once_per_run() {
    run(async {
        let state_dir = tempfile::tempdir().expect("state dir");
        let options = ContinuityOptions {
            state_dir: state_dir.path().to_path_buf(),
            execution: true,
            checkpoints: false,
            background: false,
            acceptance: false,
            ..ContinuityOptions::new(state_dir.path())
        };
        let harness = create_harness(HarnessOptions {
            continuity: Some(options),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![faux_text_response("first done")]);
        harness
            .session
            .prompt("hello".to_string(), Default::default())
            .await
            .expect("first prompt");

        let executions = state_dir.path().join("executions");
        let state_file = std::fs::read_dir(&executions)
            .expect("executions dir")
            .flatten()
            .map(|entry| entry.path())
            .find(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
            .expect("state file");
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&state_file).expect("state")).expect("json");
        assert!(
            value.get("claim").is_none_or(serde_json::Value::is_null),
            "claim must be released at run terminal: {value}"
        );
        assert_eq!(value["terminalSeq"].as_u64(), Some(1), "{value}");
        let terminal_events = value["events"]
            .as_array()
            .expect("events")
            .iter()
            .filter(|event| event["kind"] == "terminal")
            .count();
        assert_eq!(
            terminal_events, 1,
            "terminal event is published exactly once"
        );

        // 第二次 run 重新取得 claim 并在结束时再次释放。
        harness.set_responses(vec![faux_text_response("second done")]);
        harness
            .session
            .prompt("again".to_string(), Default::default())
            .await
            .expect("second prompt");
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&state_file).expect("state")).expect("json");
        assert!(
            value.get("claim").is_none_or(serde_json::Value::is_null),
            "{value}"
        );
        assert_eq!(value["terminalSeq"].as_u64(), Some(2), "{value}");
        harness.cleanup();
    })
    .await;
}

/// `bash background=true`：父 turn 立即拿到 task id；任务在 turn 结束后继续；终态通知
/// 在下一轮 run 作为自定义消息进入请求。
#[tokio::test]
async fn bash_background_returns_task_id_and_delivers_terminal_notification() {
    run(async {
        let state_dir = tempfile::tempdir().expect("state dir");
        let options = ContinuityOptions {
            state_dir: state_dir.path().to_path_buf(),
            execution: false,
            checkpoints: false,
            background: true,
            acceptance: false,
            ..ContinuityOptions::new(state_dir.path())
        };
        let harness = create_harness(HarnessOptions {
            continuity: Some(options),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            faux_tool_response(
                tool_call(
                    "call-bg",
                    "bash",
                    json!({
                        "command": "sleep 0.2; echo background-done",
                        "background": true
                    }),
                ),
                StopReason::ToolUse,
            ),
            faux_text_response("started"),
        ]);
        harness
            .session
            .prompt("start it".to_string(), Default::default())
            .await
            .expect("prompt");

        // 父 turn 已结束，但任务还在运行/刚结束；解析工具结果里的 task id。
        let messages = serde_json::to_string(&harness.session.messages()).expect("messages");
        let marker = "Background task started: ";
        let start = messages.find(marker).expect("background task id in tool result") + marker.len();
        let task_id: String = messages[start..]
            .chars()
            .take_while(|character| character.is_ascii_alphanumeric() || *character == '_' || *character == '-')
            .collect();
        assert!(task_id.starts_with("task_"), "{task_id}");

        let manager = harness
            .session
            .background_task_manager()
            .expect("background manager")
            .clone();
        let mut record = None;
        let mut last_seen = None;
        for _ in 0..100 {
            match manager.task(&task_id).await {
                Ok(Some(found)) => {
                    last_seen = Some(format!("{:?}", found.status));
                    if found.is_terminal() && found.notified {
                        record = Some(found);
                        break;
                    }
                }
                Ok(None) => last_seen = Some("missing".to_string()),
                Err(error) => last_seen = Some(format!("error: {error}")),
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let record = record.unwrap_or_else(|| {
            panic!(
                "background task completes after the parent turn (last={last_seen:?}, messages={})",
                serde_json::to_string(&harness.session.messages()).expect("messages")
            )
        });
        assert_eq!(record.status.as_str(), "completed", "{record:?}");
        // 后台完成时也执行验收：即使没有 criterion，也必须留下显式报告（不会静默通过）。
        let terminal = record.terminal.as_ref().expect("terminal");
        let acceptance = terminal
            .acceptance
            .as_ref()
            .expect("background completion carries an acceptance report");
        assert_eq!(acceptance.status, kiana_session::acceptance::AcceptanceStatus::Unverified);
        let output = std::fs::read_to_string(&record.output_path).expect("output file");
        assert!(output.contains("background-done"), "{output:?}");
        assert!(
            harness.session.pending_background_notifications() >= 1,
            "terminal notification is queued for the parent session (notified={}, last_error={:?}, terminal={:?})",
            record.notified,
            manager.last_error(),
            record.terminal
        );

        // 下一轮 run 开始时通知作为自定义消息注入并持久化。
        harness.set_responses(vec![faux_text_response("noted")]);
        harness
            .session
            .prompt("continue".to_string(), Default::default())
            .await
            .expect("second prompt");
        let messages = serde_json::to_string(&harness.session.messages()).expect("messages");
        assert!(messages.contains("kiana.background-task"), "{messages}");
        assert!(messages.contains(&task_id), "{messages}");
        // custom-role 投影只接受字符串/数组内容；object 会变成空 user turn。
        let value: serde_json::Value =
            serde_json::from_str(&messages).expect("messages JSON");
        let custom = value
            .as_array()
            .expect("messages")
            .iter()
            .find(|message| message["customType"] == "kiana.background-task")
            .expect("background notification message");
        assert!(
            custom["content"].is_string(),
            "notification content must be a string: {custom}"
        );
        assert_eq!(custom["details"]["taskId"].as_str(), Some(task_id.as_str()));
        assert_eq!(custom["details"]["terminalSeq"].as_u64(), Some(1));
        assert_eq!(
            harness.session.pending_background_notifications(),
            0,
            "notification is consumed exactly once"
        );
        harness.cleanup();
    })
    .await;
}

/// DegradedHistory：会话打开后任何 `refreshContext()`（树导航/压缩路径）都必须重新应用
/// 请求视图修复，孤儿 tool_result 不能重新进入 agent 历史。
#[tokio::test]
async fn degraded_history_repairs_survive_later_context_refreshes() {
    run(async {
        let mut manager = SessionManager::in_memory("/tmp/kiana-degraded-refresh", None, None)
            .expect("in-memory session");
        manager
            .append_message(user_message("hello", 1))
            .expect("user");
        manager
            .append_message(tool_result_message("orphan-call", 2))
            .expect("orphan result");
        let harness = create_harness(HarnessOptions {
            session_manager: Some(manager),
            ..Default::default()
        })
        .await;
        assert_eq!(
            harness.session.recovery_classification(),
            Some(RecoveryClass::DegradedHistory)
        );
        assert!(
            !harness
                .session
                .messages()
                .iter()
                .any(|message| message.role() == "toolResult"),
            "orphan tool result is removed from the request view on open"
        );
        // 模拟树导航/压缩后的刷新。
        harness.session.refresh_context();
        assert!(
            !harness
                .session
                .messages()
                .iter()
                .any(|message| message.role() == "toolResult"),
            "refresh must re-apply request view repairs"
        );
        harness.cleanup();
    })
    .await;
}

/// InterruptedPrompt：启动路径消费 continuation，恰好重放一次，canonical 不新增 user entry。
#[tokio::test]
async fn interrupted_prompt_replays_once_without_a_new_user_entry() {
    run(async {
        let manager = manager_with(vec![user_message("finish the task", 1)]);
        let harness = create_harness(HarnessOptions {
            session_manager: Some(manager),
            ..Default::default()
        })
        .await;
        let users_before = harness
            .session
            .session_manager()
            .get_branch(None)
            .iter()
            .filter(|entry| match entry {
                kiana_session_core::session_manager::SessionEntry::Message(message) => {
                    message.message.role() == "user"
                }
                _ => false,
            })
            .count();
        harness.set_responses(vec![faux_text_response("finished")]);
        assert!(
            harness
                .session
                .resume_recovery_continuation()
                .await
                .expect("replay"),
            "first call replays"
        );
        assert!(
            !harness
                .session
                .resume_recovery_continuation()
                .await
                .expect("second call"),
            "continuation is delivered once"
        );
        let users_after = harness
            .session
            .session_manager()
            .get_branch(None)
            .iter()
            .filter(|entry| match entry {
                kiana_session_core::session_manager::SessionEntry::Message(message) => {
                    message.message.role() == "user"
                }
                _ => false,
            })
            .count();
        assert_eq!(
            users_after, users_before,
            "replay must not append a new canonical user entry"
        );
        assert!(
            harness
                .session
                .messages()
                .iter()
                .any(|message| message.role() == "assistant"),
            "the replayed prompt was answered"
        );
        harness.cleanup();
    })
    .await;
}

fn tool_result_message(tool_call_id: &str, timestamp: i64) -> kiana_agent_core::AgentMessage {
    use kiana_ai::types::{ToolResultMessage, ToolResultRole};
    kiana_agent_core::AgentMessage::Standard(Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: tool_call_id.to_string(),
        tool_name: "write".to_string(),
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "orphan".to_string(),
            text_signature: None,
        })],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp,
        duration_ms: None,
    }))
}

/// checkpoint prune 在生产路径：`ContinuityRuntime::begin_session` 会话启动时按
/// RetentionPolicy 清理可证明已删除的项目，活跃项目保留。
#[tokio::test]
async fn session_startup_prunes_orphaned_checkpoints_but_keeps_active_ones() {
    run(async {
        let state_dir = tempfile::tempdir().expect("state dir");
        let manager = kiana_session::checkpoint::CheckpointManager::new(
            state_dir.path().join("checkpoints"),
            kiana_session::checkpoint::CheckpointOptions::default(),
        );
        let root = state_dir.path().join("projects-root");
        std::fs::create_dir_all(&root).expect("root");
        let deleted = root.join("deleted");
        std::fs::create_dir_all(&deleted).expect("deleted project");
        std::fs::write(deleted.join("a.txt"), "a\n").expect("file");
        manager
            .ensure_checkpoint(&deleted, "turn-1")
            .expect("checkpoint");
        let active = root.join("active");
        std::fs::create_dir_all(&active).expect("active project");
        std::fs::write(active.join("a.txt"), "a\n").expect("file");
        manager
            .ensure_checkpoint(&active, "turn-1")
            .expect("checkpoint");

        // 删除项目并留下非空父目录（证明“目录被删除”而不是“卷未挂载”）。
        std::fs::remove_dir_all(&deleted).expect("delete project");
        std::fs::write(root.join("sibling.txt"), "sibling").expect("sibling");

        // 把被删除项目的 lastTouchMs 改成 0，确保 retention 过期。
        let checkpoints = state_dir.path().join("checkpoints");
        for project in std::fs::read_dir(checkpoints.join("projects")).expect("projects") {
            let project = project.expect("entry").path();
            let Some(meta_dir) = project.join("meta").is_dir().then(|| project.join("meta")) else {
                continue;
            };
            for meta in std::fs::read_dir(&meta_dir).expect("meta") {
                let path = meta.expect("meta entry").path();
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let mut value: serde_json::Value = serde_json::from_str(&text).expect("meta json");
                if value["workdir"]
                    .as_str()
                    .is_some_and(|workdir| workdir.contains("deleted"))
                {
                    value["lastTouchMs"] = serde_json::json!(0);
                    std::fs::write(&path, serde_json::to_vec(&value).expect("json"))
                        .expect("write");
                }
            }
        }

        let runtime = kiana_session::continuity::ContinuityRuntime::new(ContinuityOptions {
            execution: false,
            checkpoints: true,
            background: false,
            acceptance: false,
            ..ContinuityOptions::new(state_dir.path())
        });
        runtime.begin_session("prune-session").expect("begin");
        assert!(
            runtime
                .diagnostics()
                .iter()
                .any(|line| line.contains("checkpoint prune")),
            "{:?}",
            runtime.diagnostics()
        );
        let remaining: Vec<_> = std::fs::read_dir(checkpoints.join("projects"))
            .expect("projects")
            .flatten()
            .map(|entry| entry.path())
            .collect();
        // 活跃项目保留；被删除项目被移除（只可能剩下 active 的目录）。
        assert_eq!(remaining.len(), 1, "{remaining:?}");
        assert!(active.exists(), "active workdir untouched");
    })
    .await;
}

/// 启动 sweep（生产入口 `AgentSession::sweep_background_tasks`）：上个进程崩溃留下的
/// Running durable 记录必须 terminalize 并投递一次终态通知。
#[tokio::test]
async fn startup_sweep_terminalizes_durable_records_from_a_crashed_process() {
    use kiana_session::background::DurableTaskJournal;
    use kiana_session::background::journal::{
        BackgroundTaskJournal, BackgroundTaskRecord, BackgroundTaskStatus,
    };

    run(async {
        let state_dir = tempfile::tempdir().expect("state dir");
        let output_path = state_dir.path().join("crashed.output");
        std::fs::write(&output_path, "partial").expect("output");
        // 模拟进程 A 保存 Running 记录后崩溃（不调用 finish）。
        let journal = DurableTaskJournal::new(state_dir.path().join("background"));
        journal
            .save(BackgroundTaskRecord {
                version: 1,
                task_id: "task_crashed".to_string(),
                parent_session_id: "crashed-session".to_string(),
                parent_turn_id: "turn-1".to_string(),
                tool: "bash".to_string(),
                description: None,
                status: BackgroundTaskStatus::Running,
                terminal_seq: 0,
                terminal: None,
                output_path: output_path.to_string_lossy().into_owned(),
                output_bytes: 7,
                output_truncated: false,
                notified: false,
                attempts: 1,
                created_at_ms: 1,
                updated_at_ms: 1,
                pid: Some(std::process::id()),
                work_backed: false,
                work_id: None,
                run_id: None,
            })
            .await
            .expect("save");

        // 进程 B：新会话使用同一个 state dir，启动 sweep 接管 durable 记录。
        let harness = create_harness(HarnessOptions {
            continuity: Some(ContinuityOptions {
                state_dir: state_dir.path().to_path_buf(),
                execution: false,
                checkpoints: false,
                background: true,
                acceptance: false,
                ..ContinuityOptions::new(state_dir.path())
            }),
            ..Default::default()
        })
        .await;
        let report = harness
            .session
            .sweep_background_tasks()
            .await
            .expect("sweep")
            .expect("background enabled");
        assert_eq!(report.terminalized, vec!["task_crashed".to_string()]);
        let manager = harness
            .session
            .background_task_manager()
            .expect("background manager")
            .clone();
        let record = manager
            .task("task_crashed")
            .await
            .expect("load")
            .expect("record");
        assert!(record.is_terminal());
        assert_eq!(record.status, BackgroundTaskStatus::Interrupted);
        assert!(record.notified);
        assert!(harness.session.pending_background_notifications() >= 1);
        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn explicit_settings_disable_keeps_continuity_off() {
    run(async {
        // `continuity: false` 由 sdk 解析成 `continuity_disabled`：即使没有显式
        // ContinuityOptions，也不能再走环境变量回退。
        let harness = create_harness(HarnessOptions {
            continuity_disabled: true,
            ..Default::default()
        })
        .await;
        assert!(!harness.session.continuity_enabled());
        harness.cleanup();
    })
    .await;
}
