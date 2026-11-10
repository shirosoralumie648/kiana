//! 子代理生产入口（`AgentSession::spawn_subagent` / `run_subagent`）。
//!
//! 覆盖 audit 发现 5：运行时之外必须有一个驱动模型循环的生产入口。这里用 faux
//! provider 走真实的 `Agent` 循环（工具调用 → 结果 → 下一轮），并断言：
//! * 终态、transcript、usage、receipts 都来自真实事件，而不是测试手工写入；
//! * `maxTurns` 超限会中止 run 并写 `turn_capped`；
//! * yield 契约的装配输出进入终态；
//! * `spawn_named_subagent` 能从内建定义加载并完成一次 run。

mod common;

use std::sync::Arc;

use kiana_agent_core::types::{
    AgentTool, AgentToolError, AgentToolExecute, AgentToolResult, BoxFuture,
};
use kiana_ai::compat::{ApiProvider, register_api_provider};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, RegisterFauxProviderOptions, create_faux_core,
    faux_assistant_message, faux_tool_call,
};
use kiana_ai::types::{
    JsonObject, JsonValue, Model, StopReason, TextContent, TextKind, UserContentBlock,
};
use kiana_session::agent_definition::builtin_agent_definitions;
use kiana_session::agent_session::stream_simple_stream_fn;
use kiana_session::subagent::{
    IsolationMode, IsolationOptions, SubagentParentContext, SubagentRunOptions, SubagentRunOutcome,
    SubagentRuntime, SubagentRuntimeOptions, SubagentStopReason, SubagentTerminal,
    SubagentTerminalState, ToolPool, YieldContract, run_subagent,
};
use serde_json::json;

struct EchoExecute;

impl AgentToolExecute for EchoExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: JsonValue,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        Box::pin(async move {
            Ok(AgentToolResult {
                content: vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: format!("echo: {params}"),
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

fn echo_tool() -> AgentTool {
    AgentTool {
        name: "echo".to_string(),
        label: "Echo".to_string(),
        description: "echo back the arguments".to_string(),
        parameters: json!({"type": "object"}),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(EchoExecute),
    }
}

fn registration() -> (Arc<kiana_ai::providers::faux::FauxCore>, Model) {
    let core = create_faux_core(RegisterFauxProviderOptions {
        ..Default::default()
    });
    let faux = Arc::new(core.clone());
    register_api_provider(ApiProvider::from_streams(faux.api(), Arc::new(core)), None);
    let model = faux.models().first().cloned().expect("faux model");
    (faux, model)
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

fn general_purpose() -> kiana_session::agent_definition::AgentDefinition {
    builtin_agent_definitions()
        .into_iter()
        .find(|definition| definition.name == "general-purpose")
        .expect("builtin general-purpose")
}

fn runtime_with_tools(
    options: SubagentRuntimeOptions,
    tools: Vec<AgentTool>,
) -> Arc<SubagentRuntime> {
    let parent = SubagentParentContext::new(
        "parent-session",
        std::env::temp_dir().join("kiana-runner-parent"),
        ToolPool::new(tools),
    );
    Arc::new(SubagentRuntime::prepare(general_purpose(), parent, options).expect("prepare"))
}

fn runtime_with_echo_tool(options: SubagentRuntimeOptions) -> Arc<SubagentRuntime> {
    runtime_with_tools(options, vec![echo_tool()])
}

fn echo_tool_named(name: &str) -> AgentTool {
    let mut tool = echo_tool();
    tool.name = name.to_string();
    tool
}

fn run_options(model: Model, prompt: &str) -> SubagentRunOptions {
    SubagentRunOptions {
        model,
        stream_fn: Some(stream_simple_stream_fn()),
        prompt: prompt.to_string(),
        cancel: None,
        session_id: None,
    }
}

async fn assert_completed(outcome: &SubagentRunOutcome) {
    assert_eq!(
        outcome.terminal.terminal,
        SubagentTerminal::Completed,
        "{:?}",
        outcome.terminal
    );
}

#[tokio::test]
async fn runner_drives_the_model_loop_and_collects_terminal_usage_receipts() {
    let (faux, model) = registration();
    faux.set_responses(vec![
        tool_call_message("echo", json!({"text": "hello"})),
        text_message("all done"),
    ]);
    let session_dir = tempfile::tempdir().expect("temp dir");
    let runtime = runtime_with_echo_tool(SubagentRuntimeOptions {
        session_dir: Some(session_dir.path().to_path_buf()),
        session_id: Some("child-runner".to_string()),
        ..Default::default()
    });
    let outcome = run_subagent(runtime.clone(), run_options(model, "use echo please"))
        .await
        .expect("run subagent");

    assert_completed(&outcome).await;
    assert_eq!(outcome.terminal.result.as_deref(), Some("all done"));
    // 用户消息 + assistant(tool call) + tool result + assistant(final) 都进了 transcript。
    assert!(outcome.messages.len() >= 4, "{:?}", outcome.messages);
    assert!(
        outcome
            .messages
            .iter()
            .any(|message| message
                .as_standard()
                .is_some_and(|message| serde_json::to_string(message)
                    .unwrap_or_default()
                    .contains("echo:"))),
        "tool output missing from transcript"
    );
    assert!(outcome.usage.total_tokens > 0, "usage: {:?}", outcome.usage);
    assert_eq!(outcome.receipts.len(), 1, "{:?}", outcome.receipts);
    assert_eq!(outcome.receipts[0].tool, "echo");
    let session_file = outcome.session_file.clone().expect("persistent session");
    assert!(
        std::path::Path::new(&session_file).exists(),
        "{session_file}"
    );
    // 运行时的 transcript 与返回的消息一致（增量落盘生效）。
    assert_eq!(runtime.messages().len(), outcome.messages.len());
}

#[tokio::test]
async fn runner_aborts_and_terminalizes_when_max_turns_is_exceeded() {
    let (faux, model) = registration();
    faux.set_responses(vec![
        tool_call_message("echo", json!({"n": 1})),
        tool_call_message("echo", json!({"n": 2})),
        text_message("never reached"),
    ]);
    let mut definition = general_purpose();
    definition.max_turns = Some(1);
    let parent = SubagentParentContext::new(
        "parent-session",
        std::env::temp_dir().join("kiana-runner-parent"),
        ToolPool::new(vec![echo_tool()]),
    );
    let runtime = Arc::new(
        SubagentRuntime::prepare(definition, parent, SubagentRuntimeOptions::default())
            .expect("prepare"),
    );
    let outcome = run_subagent(runtime, run_options(model, "loop"))
        .await
        .expect("run subagent");

    assert_eq!(outcome.terminal.terminal, SubagentTerminal::Failed);
    assert_eq!(
        outcome.terminal.stop_reason,
        Some(SubagentStopReason::TurnCapped)
    );
    // 只有第一轮的工具执行了；第二轮开始前被中止。
    assert_eq!(outcome.receipts.len(), 1, "{:?}", outcome.receipts);
}

#[tokio::test]
async fn runner_surfaces_yield_output_and_persists_the_session() {
    let (faux, model) = registration();
    faux.set_responses(vec![
        tool_call_message("yield", json!({"summary": "ok"})),
        text_message("done"),
    ]);
    let session_dir = tempfile::tempdir().expect("temp dir");
    let contract = YieldContract::new(json!({
        "type": "object",
        "required": ["summary"],
        "properties": {"summary": {"type": "string"}}
    }));
    let runtime = runtime_with_echo_tool(SubagentRuntimeOptions {
        session_dir: Some(session_dir.path().to_path_buf()),
        session_id: Some("child-yield".to_string()),
        yield_contract: Some(contract),
        ..Default::default()
    });
    let outcome = run_subagent(runtime, run_options(model, "yield a summary"))
        .await
        .expect("run subagent");

    assert_completed(&outcome).await;
    assert_eq!(outcome.output, Some(json!({"summary": "ok"})));
    assert_eq!(
        outcome.terminal.result.as_deref(),
        Some("{\"summary\":\"ok\"}")
    );
}

#[tokio::test]
async fn runner_fails_when_a_yield_contract_is_left_incomplete() {
    let (faux, model) = registration();
    faux.set_responses(vec![
        tool_call_message("yield", json!({"labels": {"findings": ["a"]}})),
        text_message("done"),
    ]);
    let contract = YieldContract::new(json!({
        "type": "object",
        "required": ["summary", "findings"],
        "properties": {
            "summary": {"type": "string"},
            "findings": {"type": "array", "items": {"type": "string"}}
        }
    }));
    let runtime = runtime_with_echo_tool(SubagentRuntimeOptions {
        yield_contract: Some(contract),
        ..Default::default()
    });
    let outcome = run_subagent(runtime, run_options(model, "yield something"))
        .await
        .expect("run subagent");

    assert_eq!(outcome.terminal.terminal, SubagentTerminal::Failed);
    assert!(
        outcome
            .terminal
            .error
            .as_deref()
            .is_some_and(|error| error.contains("schema validation")),
        "incomplete yield must fail the run: {:?}",
        outcome.terminal
    );
}

#[tokio::test]
async fn runner_fails_an_empty_yield_submission_for_a_required_contract() {
    let (faux, model) = registration();
    // `yield {}` 是“调用过 yield 但没给任何字段”，结束时必须按 required 缺失判失败，
    // 不能静默回退成 assistant 文本。
    faux.set_responses(vec![
        tool_call_message("yield", json!({})),
        text_message("done"),
    ]);
    let contract = YieldContract::new(json!({
        "type": "object",
        "required": ["summary"],
        "properties": {"summary": {"type": "string"}}
    }));
    let runtime = runtime_with_echo_tool(SubagentRuntimeOptions {
        yield_contract: Some(contract),
        ..Default::default()
    });
    let outcome = run_subagent(runtime, run_options(model, "yield something"))
        .await
        .expect("run subagent");

    assert_eq!(outcome.terminal.terminal, SubagentTerminal::Failed);
    assert!(
        outcome
            .terminal
            .error
            .as_deref()
            .is_some_and(|error| error.contains("schema validation")),
        "{:?}",
        outcome.terminal
    );
}

#[tokio::test]
async fn runner_reports_a_length_truncated_model_response_as_failed() {
    let (faux, model) = registration();
    faux.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        "half an answer",
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Length),
            ..Default::default()
        },
    ))]);
    let runtime = runtime_with_echo_tool(SubagentRuntimeOptions::default());
    let outcome = run_subagent(runtime, run_options(model, "say something"))
        .await
        .expect("run subagent");

    assert_eq!(outcome.terminal.terminal, SubagentTerminal::Failed);
    assert_eq!(
        outcome.terminal.stop_reason,
        Some(SubagentStopReason::ModelLengthCapped)
    );
}

#[tokio::test]
async fn agent_session_spawn_named_subagent_runs_a_builtin_definition() {
    common::run(async {
        let harness = common::create_harness(common::HarnessOptions::default()).await;
        harness.set_responses(vec![text_message("subagent finished from the named entry")]);
        let probe = harness.temp_dir.path().join("subagent-done.txt");
        std::fs::write(&probe, "ok").expect("probe");
        let outcome = harness
            .session
            .spawn_named_subagent(
                "general-purpose",
                SubagentRuntimeOptions {
                    session_id: Some("named-child".to_string()),
                    acceptance_criteria: vec!["file:subagent-done.txt exists".to_string()],
                    ..Default::default()
                },
                "say something",
                None,
            )
            .await
            .expect("spawn named subagent");
        assert_completed(&outcome).await;
        assert_eq!(
            outcome.terminal.result.as_deref(),
            Some("subagent finished from the named entry")
        );
        assert!(outcome.usage.total_tokens > 0);
        // 子代理终态自动验收：caller 的 criteria 在 `spawn_subagent` 返回前判定。
        let acceptance = outcome
            .acceptance
            .as_ref()
            .expect("subagent completion carries an acceptance report");
        assert!(acceptance.all_hold(), "{acceptance:?}");
        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn a_malformed_yield_attempt_does_not_discard_the_final_text() {
    let (faux, model) = registration();
    // 第一次 yield 是畸形 payload（label 不是字符串），从未被装配器接受；
    // 模型随后给出正常回答，不能因为 attempted=true 就丢掉它。
    faux.set_responses(vec![
        tool_call_message("yield", json!({"label": 5})),
        text_message("final answer"),
    ]);
    let runtime = runtime_with_echo_tool(SubagentRuntimeOptions {
        yield_contract: Some(YieldContract::new(json!({"type": "object"}))),
        ..Default::default()
    });
    let outcome = run_subagent(runtime, run_options(model, "answer"))
        .await
        .expect("run subagent");

    assert_completed(&outcome).await;
    assert_eq!(outcome.terminal.result.as_deref(), Some("final answer"));
}

#[tokio::test]
async fn an_accepted_empty_yield_still_wins_over_the_final_text() {
    let (faux, model) = registration();
    // `yield {}` 被接受（没有 required 字段）时是显式空结果，不回退到文本。
    faux.set_responses(vec![
        tool_call_message("yield", json!({})),
        text_message("ignored answer"),
    ]);
    let runtime = runtime_with_echo_tool(SubagentRuntimeOptions {
        yield_contract: Some(YieldContract::new(json!({"type": "object"}))),
        ..Default::default()
    });
    let outcome = run_subagent(runtime, run_options(model, "answer"))
        .await
        .expect("run subagent");

    assert_completed(&outcome).await;
    assert_eq!(outcome.terminal.result.as_deref(), Some("{}"));
}

#[cfg(unix)]
#[tokio::test]
async fn a_transcript_persist_failure_fails_the_run() {
    use std::os::unix::fs::PermissionsExt;

    let (faux, model) = registration();
    faux.set_responses(vec![text_message("answer")]);
    let temp = tempfile::tempdir().expect("temp dir");
    let session_dir = temp.path().join("sessions");
    std::fs::create_dir_all(&session_dir).expect("session dir");
    let runtime = runtime_with_echo_tool(SubagentRuntimeOptions {
        session_dir: Some(session_dir.clone()),
        session_id: Some("child-persist-failure".to_string()),
        ..Default::default()
    });
    // 落盘目标不可写：文件已存在就锁文件，否则锁目录。
    let file = runtime.session_file().expect("session file");
    let file_path = std::path::PathBuf::from(&file);
    let target = if file_path.exists() {
        file_path
    } else {
        session_dir
    };
    let original = std::fs::metadata(&target).expect("metadata").permissions();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o400)).expect("chmod");
    let outcome = run_subagent(runtime, run_options(model, "hello"))
        .await
        .expect("run subagent");
    std::fs::set_permissions(&target, original).expect("restore");

    assert_eq!(
        outcome.terminal.terminal,
        SubagentTerminal::Failed,
        "{:?}",
        outcome.terminal
    );
    assert!(
        outcome
            .terminal
            .error
            .as_deref()
            .is_some_and(|error| error.contains("persist")),
        "{:?}",
        outcome.terminal
    );
}

/// 未使用的导入守卫：`SubagentTerminalState` 在断言失败时用于调试输出。
#[allow(dead_code)]
fn _terminal_type_anchor(state: SubagentTerminalState) -> SubagentTerminalState {
    state
}

fn git(cwd: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?} failed");
}

#[tokio::test]
async fn spawn_subagent_cleans_an_unchanged_isolated_workspace() {
    common::run(async {
        let harness = common::create_harness(common::HarnessOptions::default()).await;
        let cwd = harness.temp_dir.path().to_path_buf();
        // worktree 隔离要求 git 仓库至少有一个 commit。
        git(&cwd, &["init", "-q"]);
        std::fs::write(cwd.join("seed.txt"), "seed").expect("seed");
        git(&cwd, &["add", "-A"]);
        git(
            &cwd,
            &[
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=test",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "init",
            ],
        );
        harness.set_responses(vec![text_message("isolated done")]);

        let outcome = harness
            .session
            .spawn_named_subagent(
                "general-purpose",
                SubagentRuntimeOptions {
                    isolation: Some(IsolationMode::Worktree),
                    isolation_options: IsolationOptions::default(),
                    ..Default::default()
                },
                "say something",
                None,
            )
            .await
            .expect("spawn isolated subagent");
        assert_completed(&outcome).await;
        let isolation = outcome.isolation.as_ref().expect("isolation outcome");
        assert!(
            !isolation.retained,
            "a clean worktree must not be retained: {isolation:?}"
        );

        // 无改动的隔离工作区在终态后被清理（Drop）。
        let worktrees = cwd.join(".kiana").join("subagents").join("worktrees");
        let leftover = std::fs::read_dir(&worktrees)
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(leftover, 0, "isolated worktree must be cleaned up");
        assert_eq!(
            std::fs::read_to_string(cwd.join("seed.txt")).expect("seed survives"),
            "seed"
        );
        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn an_exhausted_yield_contract_fails_instead_of_falling_back_to_text() {
    let (faux, model) = registration();
    // 模型调用过 yield，但每次提交都不符合 schema 且重试次数已经用尽：这时不能
    // 静默改用 assistant 文本，否则宿主会以为结构化契约从未被尝试。
    faux.set_responses(vec![
        tool_call_message("yield", json!({"label": "summary", "value": 5})),
        text_message("final answer"),
    ]);
    let contract = YieldContract::new(json!({
        "type": "object",
        "required": ["summary"],
        "properties": {"summary": {"type": "string"}}
    }))
    .with_max_retries(0);
    let runtime = runtime_with_echo_tool(SubagentRuntimeOptions {
        yield_contract: Some(contract),
        ..Default::default()
    });
    let outcome = run_subagent(runtime, run_options(model, "answer"))
        .await
        .expect("run subagent");

    assert_eq!(outcome.terminal.terminal, SubagentTerminal::Failed);
    assert!(
        outcome
            .terminal
            .error
            .as_deref()
            .is_some_and(|error| error.contains("schema validation")),
        "exhausted yield must fail the run: {:?}",
        outcome.terminal
    );
}

#[tokio::test]
async fn run_subagent_itself_evaluates_acceptance_and_reports_isolation() {
    let (faux, model) = registration();
    faux.set_responses(vec![text_message("done")]);
    let probe = std::env::temp_dir()
        .join("kiana-runner-parent")
        .join("acceptance-probe.txt");
    std::fs::create_dir_all(probe.parent().expect("parent")).expect("probe dir");
    std::fs::write(&probe, b"ok").expect("probe file");

    let runtime = runtime_with_echo_tool(SubagentRuntimeOptions {
        acceptance_criteria: vec!["file:acceptance-probe.txt exists".to_string()],
        ..Default::default()
    });
    let outcome = run_subagent(runtime, run_options(model, "answer"))
        .await
        .expect("run subagent");
    assert_completed(&outcome).await;

    let acceptance = outcome
        .acceptance
        .as_ref()
        .expect("run_subagent must evaluate the runtime's acceptance criteria");
    assert_eq!(
        acceptance.status,
        kiana_session::acceptance::AcceptanceStatus::Passed,
        "{acceptance:?}"
    );
    assert!(outcome.isolation.is_none());
    let _ = std::fs::remove_file(&probe);
}

#[tokio::test]
async fn a_host_preset_terminal_aborts_the_model_loop() {
    let (faux, model) = registration();
    faux.set_responses(vec![text_message("must not be recorded")]);
    let runtime = runtime_with_echo_tool(SubagentRuntimeOptions::default());
    // 宿主在运行开始前已经写终态（例如外部超时）：TurnStart 必须中止 agent，
    // 不能继续产生归属不明的消息。
    assert!(runtime.timeout(), "terminal must be unset before the run");
    let outcome = run_subagent(runtime, run_options(model, "answer"))
        .await
        .expect("run subagent");
    assert_eq!(
        outcome.terminal.terminal,
        SubagentTerminal::TimedOut,
        "{:?}",
        outcome.terminal
    );
}

#[tokio::test]
async fn a_pool_tool_named_yield_does_not_shadow_the_contract_tool() {
    let (faux, model) = registration();
    // 父工具池里同名的 `yield` 不能遮蔽契约装配器：agent loop 按第一个同名工具解析。
    faux.set_responses(vec![
        tool_call_message("yield", json!({"summary": "structured"})),
        text_message("prose fallback"),
    ]);
    let runtime = runtime_with_tools(
        SubagentRuntimeOptions {
            yield_contract: Some(YieldContract::new(json!({
                "type": "object",
                "required": ["summary"],
                "properties": {"summary": {"type": "string"}}
            }))),
            ..Default::default()
        },
        vec![echo_tool_named("yield")],
    );
    let outcome = run_subagent(runtime, run_options(model, "answer"))
        .await
        .expect("run subagent");

    assert_completed(&outcome).await;
    assert_eq!(
        outcome.output,
        Some(json!({"summary": "structured"})),
        "the contract yield tool must win over a same-named pool tool"
    );
}

#[tokio::test]
async fn retention_failure_is_reported_in_the_isolation_outcome() {
    let (faux, model) = registration();
    let temp = tempfile::tempdir().expect("temp dir");
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo");
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("seed.txt"), "seed").expect("seed");
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );

    let state = temp.path().join("state");
    let parent = SubagentParentContext::new("parent", &repo, ToolPool::new(vec![echo_tool()]));
    let runtime = SubagentRuntime::prepare(
        general_purpose(),
        parent,
        SubagentRuntimeOptions {
            isolation: Some(IsolationMode::Worktree),
            isolation_options: IsolationOptions {
                state_dir: Some(state.clone()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .expect("prepare");
    let workspace = runtime
        .isolated_workspace()
        .expect("isolated")
        .root()
        .to_path_buf();
    // 让工作区产生改动，终态时才会尝试 retain。
    std::fs::write(workspace.join("change.txt"), b"change").expect("dirty workspace");
    // 把 ownership 目录替换成普通文件：记录写入必然失败。
    std::fs::remove_dir_all(state.join("ownership")).expect("remove ownership dir");
    std::fs::write(state.join("ownership"), b"not a dir").expect("blocking file");

    faux.set_responses(vec![text_message("done")]);
    let outcome = run_subagent(Arc::new(runtime), run_options(model, "answer"))
        .await
        .expect("run subagent");
    let isolation = outcome.isolation.as_ref().expect("isolation outcome");
    assert!(!isolation.retained);
    assert!(
        isolation.retain_error.is_some(),
        "a changed workspace whose record could not be written must not look unchanged"
    );
}

#[tokio::test]
async fn an_exhausted_yield_without_required_fields_still_fails() {
    let (faux, model) = registration();
    // 契约没有 required 字段：空对象也能通过 finish()，但模型确实提交过非法
    // yield 且重试耗尽，必须判失败，而不是以 output={} 成功并丢掉正文。
    faux.set_responses(vec![
        tool_call_message("yield", json!({"label": "summary", "value": 5})),
        text_message("final answer"),
    ]);
    let contract = YieldContract::new(json!({
        "type": "object",
        "properties": {"summary": {"type": "string"}}
    }))
    .with_max_retries(0);
    let runtime = runtime_with_echo_tool(SubagentRuntimeOptions {
        yield_contract: Some(contract),
        ..Default::default()
    });
    let outcome = run_subagent(runtime, run_options(model, "answer"))
        .await
        .expect("run subagent");

    assert_eq!(outcome.terminal.terminal, SubagentTerminal::Failed);
    assert!(
        outcome
            .terminal
            .error
            .as_deref()
            .is_some_and(|error| error.contains("schema validation")),
        "exhausted yield must not complete with an empty object: {:?}",
        outcome.terminal
    );
    assert!(outcome.output.is_none());
}
