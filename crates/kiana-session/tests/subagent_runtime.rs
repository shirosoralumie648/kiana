//! 子代理运行时的端到端用例。
//!
//! 覆盖 DoD：
//! - 子代理 allow `{*}`、父 allow `{read}` → 有效权限 `{read}`；
//! - 父 deny 的工具从 schema 消失，直接 dispatch 也失败；
//! - headless 子代理 Ask 为 Deny；
//! - 工具 schema 与 dispatch 只含 allowlist；
//! - unknown frontmatter 字段报错但不中止目录；
//! - resume 复用 transcript 且消息数不翻倍；
//! - terminal 竞争只有一个赢家；
//! - isolation baseline/delta 用 `git apply` 字节一致重放。

mod common;

use std::sync::Arc;

use kiana_agent_core::types::{
    AgentTool, AgentToolError, AgentToolExecute, AgentToolResult, BoxFuture,
};
use kiana_ai::types::{JsonValue, TextContent, TextKind, UserContentBlock};
use kiana_permissions::PermissionPolicy;
use kiana_session::agent_definition::{
    AgentDefinitionLoader, AgentDefinitionRoot, AgentDefinitionSource, builtin_agent_definitions,
};
use kiana_session::subagent::{
    CanonicalToolId, IsolationMode, IsolationOptions, SubagentApprovalMode, SubagentParentContext,
    SubagentRuntime, SubagentRuntimeOptions, SubagentStopReason, SubagentStreamChunk,
    SubagentTerminal, SubagentTerminalState, TerminalCell, ToolPattern, ToolPermissionSet,
    ToolPool, apply_delta, capture_baseline, capture_delta,
};
use serde_json::json;

struct EchoExecute;

impl AgentToolExecute for EchoExecute {
    fn execute<'a>(
        &'a self,
        tool_call_id: &'a str,
        params: JsonValue,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        Box::pin(async move {
            Ok(AgentToolResult {
                content: vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: format!("{tool_call_id} ok"),
                    text_signature: None,
                })],
                details: params,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

fn tool(name: &str) -> AgentTool {
    AgentTool {
        name: name.to_string(),
        label: name.to_string(),
        description: format!("{name} tool"),
        parameters: json!({"type": "object"}),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(EchoExecute),
    }
}

fn read_only_parent() -> SubagentParentContext {
    let tools = ToolPool::new(vec![tool("read"), tool("bash"), tool("write")]);
    SubagentParentContext::new("parent-session", "/tmp/kiana-week11", tools)
        .with_allowed_tools(ToolPermissionSet::from_ids([CanonicalToolId::parse(
            "read",
        )]))
        .with_permission_policy(PermissionPolicy::new(
            kiana_permissions::PermissionMode::Default,
            "/tmp/kiana-week11",
        ))
}

fn general_purpose() -> kiana_session::agent_definition::AgentDefinition {
    builtin_agent_definitions()
        .into_iter()
        .find(|definition| definition.name == "general-purpose")
        .expect("builtin")
}

#[tokio::test]
async fn child_star_is_intersected_with_parent_read_only() {
    let runtime = SubagentRuntime::prepare(
        general_purpose(),
        read_only_parent(),
        SubagentRuntimeOptions {
            approval: SubagentApprovalMode::Interactive,
            ..Default::default()
        },
    )
    .expect("prepare");
    assert_eq!(runtime.tool_names(), vec!["read"]);
    assert_eq!(runtime.tool_schemas().len(), 1);
    assert_eq!(runtime.tool_schemas()[0].name, "read");

    // bash 不在有效集合里：即使绕过模型直接 dispatch 也失败。
    let error = runtime
        .dispatch_tool("call-1", "bash", json!({"command": "echo hi"}))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        kiana_session::subagent::SubagentError::ToolUnavailable { .. }
    ));
}

#[tokio::test]
async fn parent_denied_tool_is_missing_from_schema_and_dispatch() {
    let mut definition = general_purpose();
    definition.disallowed_tools = vec![ToolPattern::new("bash")];
    let runtime = SubagentRuntime::prepare(
        definition,
        read_only_parent(),
        SubagentRuntimeOptions {
            approval: SubagentApprovalMode::Interactive,
            ..Default::default()
        },
    )
    .expect("prepare");
    assert!(!runtime.tool_names().iter().any(|name| name == "bash"));
    assert!(
        !runtime
            .tool_schemas()
            .iter()
            .any(|schema| schema.name == "bash")
    );
    let error = runtime
        .dispatch_tool("call-1", "bash", json!({"command": "echo hi"}))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        kiana_session::subagent::SubagentError::ToolUnavailable { .. }
    ));
}

#[tokio::test]
async fn headless_subagent_turns_ask_into_deny() {
    // 父策略 Default：bash 需要 Ask。
    let parent = SubagentParentContext::new(
        "parent-session",
        "/tmp/kiana-week11",
        ToolPool::new(vec![tool("bash")]),
    )
    .with_permission_policy(PermissionPolicy::new(
        kiana_permissions::PermissionMode::Default,
        "/tmp/kiana-week11",
    ));
    let definition = builtin_agent_definitions()
        .into_iter()
        .find(|definition| definition.name == "general-purpose")
        .expect("builtin");
    let runtime = SubagentRuntime::prepare(
        definition,
        parent,
        SubagentRuntimeOptions {
            approval: SubagentApprovalMode::Headless,
            ..Default::default()
        },
    )
    .expect("prepare");
    let error = runtime
        .dispatch_tool("call-1", "bash", json!({"command": "echo hi"}))
        .await
        .unwrap_err();
    match error {
        kiana_session::subagent::SubagentError::PermissionDenied { reason, .. } => {
            assert!(reason.contains("headless"), "{reason}");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn unknown_frontmatter_field_does_not_abort_directory() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path().join("agents");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(
        dir.join("good.md"),
        "---\ndescription: good\n---\nGood prompt\n",
    )
    .expect("write");
    std::fs::write(
        dir.join("bad.md"),
        "---\ndescription: bad\nnotAField: 1\n---\nBad prompt\n",
    )
    .expect("write");

    let mut loader = AgentDefinitionLoader::new();
    loader.add_root(AgentDefinitionRoot::new(
        AgentDefinitionSource::Project,
        &dir,
    ));
    let report = loader.load();
    assert!(report.get("good").is_some());
    assert!(report.get("bad").is_none());
    assert_eq!(report.errors.len(), 1);
    assert!(
        report.errors[0]
            .message
            .contains("unknown frontmatter field")
    );
}

#[test]
fn terminal_race_has_one_winner_and_budget_cap_is_sticky() {
    let cell = Arc::new(TerminalCell::new());
    let mut handles = Vec::new();
    for index in 0..16 {
        let cell = Arc::clone(&cell);
        handles.push(std::thread::spawn(move || {
            let state = if index % 2 == 0 {
                SubagentTerminalState::completed(format!("done {index}"))
            } else {
                SubagentTerminalState::with_terminal(
                    SubagentTerminal::TimedOut,
                    Some("timeout".to_string()),
                )
            };
            cell.try_set_terminal(state)
        }));
    }
    let winners: usize = handles
        .into_iter()
        .map(|handle| handle.join().expect("thread"))
        .filter(|won| *won)
        .count();
    assert_eq!(winners, 1);
    assert!(!cell.try_set_terminal(SubagentTerminalState::failed("late")));
    assert_eq!(cell.get().expect("terminal").seq, 1);
}

#[tokio::test]
async fn subagent_budget_sets_token_capped_once() {
    let runtime = SubagentRuntime::prepare(
        general_purpose(),
        read_only_parent(),
        SubagentRuntimeOptions {
            token_budget: Some(5),
            ..Default::default()
        },
    )
    .expect("prepare");
    assert!(runtime.record_chunk(&SubagentStreamChunk::Usage {
        input_tokens: 10,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cost: 0.0,
    }));
    let state = runtime.terminal().expect("terminal");
    assert_eq!(state.terminal, SubagentTerminal::Failed);
    assert_eq!(state.stop_reason, Some(SubagentStopReason::TokenCapped));
    assert!(!runtime.complete("late success"));
    assert_eq!(
        runtime.terminal().expect("terminal").terminal,
        SubagentTerminal::Failed
    );
}

#[test]
fn resume_reuses_transcript_without_doubling_messages() {
    let temp = tempfile::tempdir().expect("tempdir");
    let parent = SubagentParentContext::new(
        "parent-session",
        temp.path(),
        ToolPool::new(vec![tool("read")]),
    );
    let mut definition = general_purpose();
    definition.initial_prompt = Some("initial prompt".to_string());
    let runtime = SubagentRuntime::prepare(
        definition,
        parent,
        SubagentRuntimeOptions {
            session_dir: Some(temp.path().join("sessions")),
            session_id: Some("child-week11".to_string()),
            ..Default::default()
        },
    )
    .expect("prepare");
    runtime
        .append_message(user_text("initial prompt"))
        .expect("append");
    runtime
        .append_message(user_text("follow-up"))
        .expect("append");
    assert_eq!(runtime.message_count(), 2);
    let path = runtime.session_file().expect("persisted session");
    drop(runtime);

    let parent = SubagentParentContext::new(
        "parent-session",
        temp.path(),
        ToolPool::new(vec![tool("read")]),
    );
    let mut definition = general_purpose();
    definition.initial_prompt = Some("initial prompt".to_string());
    let resumed =
        SubagentRuntime::resume(definition, parent, SubagentRuntimeOptions::default(), &path)
            .expect("resume");
    assert_eq!(resumed.message_count(), 2);
    // resume 本身不重放任何消息；再次 seed 初始 prompt 也因 transcript 非空而跳过。
    assert!(!resumed.seed_initial_prompt().expect("seed"));
    assert_eq!(resumed.message_count(), 2);
    resumed
        .append_message(user_text("new message"))
        .expect("append");
    assert_eq!(resumed.message_count(), 3);
}

#[test]
fn isolation_delta_round_trips_through_git_apply() {
    if !std::process::Command::new("git")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
    {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).expect("mkdir");
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["config", "user.email", "test@example.com"]);
    run_git(&repo, &["config", "user.name", "Test"]);
    std::fs::write(repo.join("tracked.txt"), b"base\n").expect("write");
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);

    let options = IsolationOptions {
        state_dir: Some(temp.path().join("state")),
        ..Default::default()
    };
    let baseline = capture_baseline(&repo, &options).expect("baseline");
    std::fs::write(repo.join("tracked.txt"), b"base\nchanged\n").expect("write");
    std::fs::write(repo.join("new.txt"), b"new\n").expect("write");
    std::fs::write(repo.join("bin.dat"), b"bin\x00ary").expect("write");
    let delta = capture_delta(&repo, &baseline, &options).expect("delta");
    assert_eq!(IsolationMode::Worktree.as_str(), "worktree");

    run_git(&repo, &["reset", "--hard", "-q", "HEAD"]);
    run_git(&repo, &["clean", "-fdq"]);
    apply_delta(&repo, &delta).expect("apply");
    assert_eq!(
        std::fs::read(repo.join("tracked.txt")).unwrap(),
        b"base\nchanged\n"
    );
    assert_eq!(std::fs::read(repo.join("new.txt")).unwrap(), b"new\n");
    assert_eq!(std::fs::read(repo.join("bin.dat")).unwrap(), b"bin\x00ary");
}

fn run_git(repo: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .expect("git available");
    assert!(status.success(), "git {args:?} failed");
}

fn user_text(text: &str) -> kiana_agent_core::types::AgentMessage {
    kiana_agent_core::types::AgentMessage::Standard(kiana_ai::types::Message::User(
        kiana_ai::types::UserMessage {
            role: kiana_ai::types::UserRole::User,
            content: kiana_ai::types::UserContent::Text(text.to_string()),
            timestamp: 0,
        },
    ))
}

#[tokio::test]
async fn agent_session_exposes_subagent_parent_context() {
    let runtime = common::run(async {
        let harness = common::create_harness(common::HarnessOptions::default()).await;
        let context = harness.session.subagent_parent_context();
        let session_id = harness
            .session
            .session_manager()
            .get_session_id()
            .to_string();
        assert_eq!(context.parent_session_id, session_id);
        assert!(!context.tools.is_empty(), "default session has tools");
        assert!(
            context.tools.get("read").is_some(),
            "read is active by default"
        );
        assert!(
            !context
                .permission_mode
                .eq(&kiana_permissions::PermissionMode::BypassPermissions)
        );
        let runtime = harness
            .session
            .prepare_subagent(
                builtin_agent_definitions()
                    .into_iter()
                    .find(|definition| definition.name == "explore")
                    .expect("explore"),
                SubagentRuntimeOptions::default(),
            )
            .expect("prepare subagent");
        assert!(
            runtime
                .tool_names()
                .iter()
                .all(|name| { matches!(name.as_str(), "read" | "ls" | "grep" | "find") })
        );
        assert!(!runtime.prompt_cache_sharing_enabled());
        harness.cleanup();
        runtime.message_count()
    })
    .await;
    assert_eq!(runtime, 0);
}
