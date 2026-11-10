//! kiana-hooks 的行为测试：退出码契约、顺序聚合、updatedInput、trust gating、
//! 超时/进程组、输出截断、async 审计、环境与递归深度。

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kiana_hooks::{
    HookAuditOutcome, HookAuditRecord, HookAuditSink, HookConfigDiagnostic, HookDecision,
    HookEvent, HookInput, HookRegistry, HookRunner, HookRunnerConfig, HookSource, HookTurnState,
    HooksConfig,
};
use serde_json::{Value, json};
use tempfile::TempDir;

fn add_source(
    registry: &mut HookRegistry,
    source: HookSource,
    hooks: Value,
    trusted: bool,
) -> Vec<HookConfigDiagnostic> {
    registry.add_source(source, HooksConfig::from_value(&hooks), trusted)
}

fn build_runner(registry: HookRegistry, project_root: &Path) -> HookRunner {
    HookRunner::new(registry, HookRunnerConfig::new(project_root)).expect("runner builds")
}

fn user_runner(hooks: Value, project_root: &Path) -> HookRunner {
    let mut registry = HookRegistry::new();
    let diagnostics = add_source(&mut registry, HookSource::User, hooks, true);
    assert!(
        diagnostics.is_empty(),
        "unexpected diagnostics: {diagnostics:?}"
    );
    build_runner(registry, project_root)
}

fn command_hook(command: &str) -> Value {
    json!({ "type": "command", "command": command })
}

fn pre_tool_input(root: &Path, tool: &str, input: Value) -> HookInput {
    HookInput::new(HookEvent::PreToolUse, "session-1", root.to_string_lossy())
        .with_tool_call(tool, input)
}

#[tokio::test]
async fn pre_tool_use_exit_two_blocks_and_exposes_stderr_once() {
    let temp = TempDir::new().unwrap();
    let runner = user_runner(
        json!({
            "PreToolUse": [{
                "matcher": "bash",
                "hooks": [command_hook("printf 'blocked by policy' >&2; exit 2")]
            }]
        }),
        temp.path(),
    );

    let outcome = runner
        .run_event(&pre_tool_input(
            temp.path(),
            "bash",
            json!({"command": "rm -rf /"}),
        ))
        .await
        .expect("hook run");

    assert_eq!(outcome.decision, HookDecision::Deny);
    assert_eq!(outcome.blocking_error(), Some("blocked by policy"));
    assert_eq!(outcome.model_feedback.len(), 0);
    assert_eq!(outcome.runs.len(), 1);
    assert_eq!(outcome.runs[0].outcome, HookAuditOutcome::Blocked);
    assert_eq!(outcome.runs[0].exit_code, Some(2));
}

#[tokio::test]
async fn matcher_filters_tool_names() {
    let temp = TempDir::new().unwrap();
    let runner = user_runner(
        json!({
            "PreToolUse": [{
                "matcher": "bash|write",
                "hooks": [command_hook("exit 2")]
            }]
        }),
        temp.path(),
    );

    let skipped = runner
        .run_event(&pre_tool_input(temp.path(), "read", json!({})))
        .await
        .unwrap();
    assert!(skipped.runs.is_empty());
    assert!(!skipped.is_denied());

    let matched = runner
        .run_event(&pre_tool_input(temp.path(), "write", json!({})))
        .await
        .unwrap();
    assert!(matched.is_denied());
}

#[tokio::test]
async fn exit_one_only_warns_and_tool_continues() {
    let temp = TempDir::new().unwrap();
    let runner = user_runner(
        json!({
            "PreToolUse": [{"hooks": [command_hook("printf 'transient failure' >&2; exit 1")]}]
        }),
        temp.path(),
    );

    let outcome = runner
        .run_event(&pre_tool_input(
            temp.path(),
            "bash",
            json!({"command": "ls"}),
        ))
        .await
        .unwrap();
    assert_eq!(outcome.decision, HookDecision::Passthrough);
    assert!(!outcome.is_denied());
    assert_eq!(outcome.runs[0].outcome, HookAuditOutcome::Warning);
    assert!(
        outcome
            .warnings
            .iter()
            .any(|warning| warning.contains("transient failure")),
        "{:?}",
        outcome.warnings
    );
}

#[tokio::test]
async fn plain_stdout_is_context_only_for_context_events() {
    let temp = TempDir::new().unwrap();

    let prompt_runner = user_runner(
        json!({"UserPromptSubmit": [{"hooks": [command_hook("printf 'remember this'")]}]}),
        temp.path(),
    );
    let prompt_outcome = prompt_runner
        .run_event(
            &HookInput::new(
                HookEvent::UserPromptSubmit,
                "s",
                temp.path().to_string_lossy(),
            )
            .with_prompt("hello"),
        )
        .await
        .unwrap();
    assert_eq!(prompt_outcome.additional_context, vec!["remember this"]);

    let tool_runner = user_runner(
        json!({"PreToolUse": [{"hooks": [command_hook("printf 'not model context'")]}]}),
        temp.path(),
    );
    let tool_outcome = tool_runner
        .run_event(&pre_tool_input(temp.path(), "bash", json!({})))
        .await
        .unwrap();
    assert!(tool_outcome.additional_context.is_empty());
    assert!(tool_outcome.runs[0].stdout.contains("not model context"));
}

#[tokio::test]
async fn json_contract_is_applied_and_additional_context_is_escaped() {
    let temp = TempDir::new().unwrap();
    let runner = user_runner(
        json!({
            "PreToolUse": [{"hooks": [command_hook(
                "printf '%s' '{\"continue\":false,\"stopReason\":\"done\",\"suppressOutput\":true,\"systemMessage\":\"warn\",\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"ask\",\"permissionDecisionReason\":\"confirm\",\"updatedInput\":{\"command\":\"ls -la\"},\"additionalContext\":\"a<b>&c\"}}'"
            )]}]
        }),
        temp.path(),
    );

    let outcome = runner
        .run_event(&pre_tool_input(
            temp.path(),
            "bash",
            json!({"command": "ls"}),
        ))
        .await
        .unwrap();
    assert_eq!(outcome.decision, HookDecision::Ask);
    assert_eq!(outcome.reason.as_deref(), Some("confirm"));
    assert!(!outcome.continue_execution);
    assert_eq!(outcome.stop_reason.as_deref(), Some("done"));
    assert!(outcome.suppress_output);
    assert_eq!(outcome.system_messages, vec!["warn"]);
    assert_eq!(outcome.updated_input, Some(json!({"command": "ls -la"})));
    assert_eq!(outcome.additional_context, vec!["a<b>&c"]);
    assert_eq!(
        outcome.additional_context_xml().as_deref(),
        Some("a&lt;b&gt;&amp;c")
    );
}

#[tokio::test]
async fn sources_run_local_then_project_then_user_and_deny_wins() {
    let temp = TempDir::new().unwrap();
    let mut registry = HookRegistry::new();
    // 故意按 user → local → project 注册，验证排序不依赖添加顺序。
    add_source(
        &mut registry,
        HookSource::User,
        json!({"UserPromptSubmit": [{"hooks": [command_hook("printf 'user'")]}]}),
        true,
    );
    add_source(
        &mut registry,
        HookSource::Local,
        json!({"UserPromptSubmit": [{"hooks": [command_hook("printf 'local'")]}]}),
        true,
    );
    add_source(
        &mut registry,
        HookSource::Project,
        json!({"UserPromptSubmit": [{"hooks": [command_hook("printf 'project'")]}]}),
        true,
    );
    let runner = build_runner(registry, temp.path());
    let outcome = runner
        .run_event(
            &HookInput::new(
                HookEvent::UserPromptSubmit,
                "s",
                temp.path().to_string_lossy(),
            )
            .with_prompt("hello"),
        )
        .await
        .unwrap();
    assert_eq!(outcome.additional_context, vec!["local", "project", "user"]);

    // user allow + project deny：最终 deny。
    let mut registry = HookRegistry::new();
    add_source(
        &mut registry,
        HookSource::User,
        json!({"PreToolUse": [{"hooks": [command_hook(
            "printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"allow\"}}'"
        )]}]}),
        true,
    );
    add_source(
        &mut registry,
        HookSource::Project,
        json!({"PreToolUse": [{"hooks": [command_hook(
            "printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"deny\",\"permissionDecisionReason\":\"project policy\"}}'"
        )]}]}),
        true,
    );
    let runner = build_runner(registry, temp.path());
    let outcome = runner
        .run_event(&pre_tool_input(temp.path(), "bash", json!({})))
        .await
        .unwrap();
    assert_eq!(outcome.decision, HookDecision::Deny);
    assert_eq!(outcome.reason.as_deref(), Some("project policy"));
}

#[tokio::test]
async fn updated_input_is_replaced_in_declaration_order() {
    let temp = TempDir::new().unwrap();
    let seen = temp.path().join("seen-input.json");
    let command = format!(
        "cat > {}; printf '%s' '{{\"hookSpecificOutput\":{{\"hookEventName\":\"PreToolUse\",\"updatedInput\":{{\"b\":3}}}}}}'",
        seen.display()
    );
    let runner = user_runner(
        json!({
            "PreToolUse": [{"hooks": [
                command_hook("printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"updatedInput\":{\"a\":2}}}'"),
                command_hook(&command)
            ]}]
        }),
        temp.path(),
    );

    let outcome = runner
        .run_event(&pre_tool_input(
            temp.path(),
            "bash",
            json!({"original": true}),
        ))
        .await
        .unwrap();

    let seen_input: Value = serde_json::from_str(&std::fs::read_to_string(&seen).unwrap()).unwrap();
    assert_eq!(seen_input["tool_input"], json!({"a": 2}));
    assert_eq!(outcome.updated_input, Some(json!({"b": 3})));

    // 后续 hook 的 deny 不能被 updatedInput 绕过。
    let temp = TempDir::new().unwrap();
    let runner = user_runner(
        json!({
            "PreToolUse": [{"hooks": [
                command_hook("printf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"updatedInput\":{\"safe\":true}}}'"),
                command_hook("exit 2")
            ]}]
        }),
        temp.path(),
    );
    let outcome = runner
        .run_event(&pre_tool_input(
            temp.path(),
            "bash",
            json!({"danger": true}),
        ))
        .await
        .unwrap();
    assert!(outcome.is_denied());
    assert_eq!(outcome.updated_input, Some(json!({"safe": true})));
}

#[tokio::test]
async fn untrusted_project_and_local_sources_are_not_loaded() {
    let temp = TempDir::new().unwrap();
    let marker = temp.path().join("project-ran");
    let mut registry = HookRegistry::new();
    let project_diags = add_source(
        &mut registry,
        HookSource::Project,
        json!({"UserPromptSubmit": [{"hooks": [command_hook(&format!("touch {}", marker.display()))]}]}),
        false,
    );
    assert!(
        project_diags
            .iter()
            .any(|diagnostic| diagnostic.message.contains("not trusted")),
        "{project_diags:?}"
    );
    let local_diags = add_source(
        &mut registry,
        HookSource::Local,
        json!({"UserPromptSubmit": [{"hooks": [command_hook(&format!("touch {}", marker.display()))]}]}),
        false,
    );
    assert!(
        local_diags
            .iter()
            .any(|diagnostic| diagnostic.message.contains("not trusted")),
        "{local_diags:?}"
    );
    // user 来源不要求 trust。
    add_source(
        &mut registry,
        HookSource::User,
        json!({"UserPromptSubmit": [{"hooks": [command_hook("printf 'user-ran'")]}]}),
        false,
    );
    assert_eq!(registry.len(), 1);

    let runner = build_runner(registry, temp.path());
    let outcome = runner
        .run_event(
            &HookInput::new(
                HookEvent::UserPromptSubmit,
                "s",
                temp.path().to_string_lossy(),
            )
            .with_prompt("hello"),
        )
        .await
        .unwrap();
    assert_eq!(outcome.additional_context, vec!["user-ran"]);
    assert!(!marker.exists());
}

#[tokio::test]
async fn deny_cap_downgrades_further_denials_to_ask() {
    let temp = TempDir::new().unwrap();
    let mut registry = HookRegistry::new();
    add_source(
        &mut registry,
        HookSource::User,
        json!({"PreToolUse": [{"hooks": [
            command_hook("exit 2"),
            command_hook("exit 2"),
            command_hook("exit 2")
        ]}]}),
        true,
    );
    let mut config = HookRunnerConfig::new(temp.path());
    config.max_denies_per_turn = 2;
    let runner = HookRunner::new(registry, config).unwrap();
    let mut state = HookTurnState::at_depth(0);

    let outcome = runner
        .run_event_with_state(&pre_tool_input(temp.path(), "bash", json!({})), &mut state)
        .await
        .unwrap();
    // 前两个 deny 已生效；第三个被降级，但这一次调用仍然是 deny（不会因为上限
    // 把已经做出的拒绝撤销）。
    assert_eq!(
        outcome.decision,
        HookDecision::Deny,
        "outcome: {outcome:?}, state: {state:?}"
    );
    assert!(outcome.deny_cap_reached);
    assert_eq!(state.deny_count, 2);
    assert!(
        outcome
            .warnings
            .iter()
            .any(|warning| warning.contains("deny cap")),
        "{:?}",
        outcome.warnings
    );

    // 同一轮的下一次调用：上限已满，hook 的 deny 降级为 ask（交给用户而不是
    // 直接拒绝），避免 hook/模型无限拉扯。
    let outcome = runner
        .run_event_with_state(&pre_tool_input(temp.path(), "bash", json!({})), &mut state)
        .await
        .unwrap();
    assert_eq!(
        outcome.decision,
        HookDecision::Ask,
        "outcome: {outcome:?}, state: {state:?}"
    );
    assert!(outcome.deny_cap_reached);
    assert_eq!(state.deny_count, 2);
}

#[tokio::test]
async fn recursion_depth_is_a_typed_error_and_depth_is_forwarded() {
    let temp = TempDir::new().unwrap();
    let mut registry = HookRegistry::new();
    add_source(
        &mut registry,
        HookSource::User,
        json!({"UserPromptSubmit": [{"hooks": [command_hook("printf '%s' \"$KIANA_HOOK_DEPTH\"")]}]}),
        true,
    );
    let mut config = HookRunnerConfig::new(temp.path());
    config.max_depth = 5;
    let runner = HookRunner::new(registry, config).unwrap();

    let mut too_deep = HookTurnState::at_depth(6);
    let error = runner
        .run_event_with_state(
            &HookInput::new(
                HookEvent::UserPromptSubmit,
                "s",
                temp.path().to_string_lossy(),
            )
            .with_prompt("hi"),
            &mut too_deep,
        )
        .await
        .expect_err("depth limit");
    assert!(matches!(
        error,
        kiana_hooks::HookError::RecursionLimit { depth: 6, limit: 5 }
    ));

    let mut state = HookTurnState::at_depth(2);
    let outcome = runner
        .run_event_with_state(
            &HookInput::new(
                HookEvent::UserPromptSubmit,
                "s",
                temp.path().to_string_lossy(),
            )
            .with_prompt("hi"),
            &mut state,
        )
        .await
        .unwrap();
    assert_eq!(outcome.additional_context, vec!["3"]);
}

#[tokio::test]
async fn cwd_and_stdin_are_pinned_to_the_project_root() {
    let temp = TempDir::new().unwrap();
    let stdin_file = temp.path().join("stdin.json");
    let command = format!("cat > {}", stdin_file.display());
    let mut registry = HookRegistry::new();
    add_source(
        &mut registry,
        HookSource::User,
        json!({"UserPromptSubmit": [{"hooks": [
            command_hook("pwd"),
            command_hook(&command)
        ]}]}),
        true,
    );
    let runner = build_runner(registry, temp.path());

    // 故意传一个错误的 cwd；runner 必须覆盖为 project_root。
    let input = HookInput::new(
        HookEvent::UserPromptSubmit,
        "s",
        "/definitely/not/the/project",
    )
    .with_prompt("hi");
    let outcome = runner.run_event(&input).await.unwrap();

    let expected = std::fs::canonicalize(temp.path()).unwrap();
    let printed = std::fs::canonicalize(outcome.additional_context[0].trim()).unwrap();
    assert_eq!(printed, expected);

    let stdin: Value =
        serde_json::from_str(&std::fs::read_to_string(&stdin_file).unwrap()).unwrap();
    assert_eq!(stdin["cwd"], temp.path().to_string_lossy().as_ref());
}

#[tokio::test]
async fn injected_hook_environment_is_visible_and_large_output_is_truncated() {
    let temp = TempDir::new().unwrap();
    let mut registry = HookRegistry::new();
    add_source(
        &mut registry,
        HookSource::User,
        json!({"UserPromptSubmit": [{"hooks": [
            command_hook("printf '%s|%s|%s' \"$KIANA_PROJECT_DIR\" \"$KIANA_HOOK_EVENT\" \"$KIANA_HOOK_SOURCE\""),
            command_hook("head -c 200000 /dev/zero | tr '\\0' x")
        ]}]}),
        true,
    );
    let runner = build_runner(registry, temp.path());
    let outcome = runner
        .run_event(
            &HookInput::new(
                HookEvent::UserPromptSubmit,
                "s",
                temp.path().to_string_lossy(),
            )
            .with_prompt("hi"),
        )
        .await
        .unwrap();

    assert_eq!(
        outcome.additional_context[0],
        format!("{}|UserPromptSubmit|user", temp.path().to_string_lossy())
    );
    let truncated = &outcome.runs[1];
    assert!(truncated.output_truncated);
    assert!(truncated.stdout.len() <= kiana_hooks::MAX_HOOK_OUTPUT_BYTES + 100);
    assert!(truncated.stdout.contains("hook output truncated"));
}

#[derive(Default)]
struct RecordingAudit {
    records: Mutex<Vec<HookAuditRecord>>,
}

impl HookAuditSink for RecordingAudit {
    fn record(&self, record: HookAuditRecord) {
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(record);
    }
}

impl RecordingAudit {
    fn snapshot(&self) -> Vec<HookAuditRecord> {
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[tokio::test]
async fn async_hooks_return_immediately_and_only_write_audit() {
    let temp = TempDir::new().unwrap();
    let marker = temp.path().join("async-done");
    let audit = Arc::new(RecordingAudit::default());
    let mut registry = HookRegistry::new();
    let command = format!(
        "sleep 1; touch {}; printf '%s' '{{\"hookSpecificOutput\":{{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"deny\"}}}}'",
        marker.display()
    );
    add_source(
        &mut registry,
        HookSource::User,
        json!({"PreToolUse": [{"hooks": [
            {"type": "command", "command": command, "async": true}
        ]}]}),
        true,
    );
    let mut config = HookRunnerConfig::new(temp.path());
    config.audit = audit.clone();
    let runner = HookRunner::new(registry, config).unwrap();

    let started = Instant::now();
    let outcome = runner
        .run_event(&pre_tool_input(
            temp.path(),
            "bash",
            json!({"command": "ls"}),
        ))
        .await
        .unwrap();
    let elapsed = started.elapsed();

    // async hook 不参与决策，且不应该等 1 秒。
    assert_eq!(outcome.decision, HookDecision::Passthrough);
    assert_eq!(outcome.runs.len(), 1);
    assert_eq!(outcome.runs[0].outcome, HookAuditOutcome::AsyncStarted);
    assert!(elapsed < Duration::from_millis(800), "elapsed: {elapsed:?}");

    // 等后台任务收尾并写审计。
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let records = audit.snapshot();
        if records
            .iter()
            .any(|record| record.asynchronous && record.outcome != HookAuditOutcome::AsyncStarted)
        {
            assert!(
                records
                    .iter()
                    .any(|record| record.outcome == HookAuditOutcome::AsyncStarted)
            );
            break;
        }
        assert!(Instant::now() < deadline, "async audit never finished");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(marker.exists());
}

#[tokio::test]
async fn timeout_kills_the_whole_process_group() {
    let temp = TempDir::new().unwrap();
    let pid_file = temp.path().join("pid");
    let command = format!("sleep 30 & echo $! > {}; wait", pid_file.display());
    let mut registry = HookRegistry::new();
    add_source(
        &mut registry,
        HookSource::User,
        json!({"PreToolUse": [{"hooks": [
            {"type": "command", "command": command, "timeout": 0.2}
        ]}]}),
        true,
    );
    let runner = build_runner(registry, temp.path());

    let started = Instant::now();
    let outcome = runner
        .run_event(&pre_tool_input(temp.path(), "bash", json!({})))
        .await
        .unwrap();
    let elapsed = started.elapsed();

    assert!(elapsed < Duration::from_secs(5), "elapsed: {elapsed:?}");
    assert_eq!(outcome.runs[0].outcome, HookAuditOutcome::Timeout);
    assert!(
        outcome
            .warnings
            .iter()
            .any(|warning| warning.contains("timed out")),
        "{:?}",
        outcome.warnings
    );

    let pid: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Path::new(&format!("/proc/{pid}")).exists() {
        assert!(
            Instant::now() < deadline,
            "background process {pid} survived the process-group kill"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn event_specific_exit_two_semantics() {
    let temp = TempDir::new().unwrap();

    // PostToolUseFailure：stderr 作为模型反馈，不改变决策。
    let runner = user_runner(
        json!({"PostToolUseFailure": [{"hooks": [command_hook("printf 'tool boom' >&2; exit 2")]}]}),
        temp.path(),
    );
    let outcome = runner
        .run_event(
            &HookInput::new(
                HookEvent::PostToolUseFailure,
                "s",
                temp.path().to_string_lossy(),
            )
            .with_tool_call("bash", json!({}))
            .with_error("boom"),
        )
        .await
        .unwrap();
    assert_eq!(outcome.decision, HookDecision::Passthrough);
    assert_eq!(outcome.model_feedback, vec!["tool boom"]);

    // SessionStart：阻塞被忽略，只记警告。
    let runner = user_runner(
        json!({"SessionStart": [{"hooks": [command_hook("printf 'ignore me' >&2; exit 2")]}]}),
        temp.path(),
    );
    let outcome = runner
        .run_event(
            &HookInput::new(HookEvent::SessionStart, "s", temp.path().to_string_lossy())
                .with_source("startup"),
        )
        .await
        .unwrap();
    assert_eq!(outcome.decision, HookDecision::Passthrough);
    assert_eq!(outcome.runs[0].outcome, HookAuditOutcome::Blocked);
    assert!(
        outcome
            .warnings
            .iter()
            .any(|warning| warning.contains("not blocking")),
        "{:?}",
        outcome.warnings
    );

    // Stop：exit 2 表示“不要结束回合”。
    let runner = user_runner(
        json!({"Stop": [{"hooks": [command_hook("printf 'keep going' >&2; exit 2")]}]}),
        temp.path(),
    );
    let outcome = runner
        .run_event(&HookInput::new(
            HookEvent::Stop,
            "s",
            temp.path().to_string_lossy(),
        ))
        .await
        .unwrap();
    assert_eq!(outcome.decision, HookDecision::Deny);
    assert_eq!(outcome.blocking_error(), Some("keep going"));

    // UserPromptSubmit：exit 2 阻止提交。
    let runner = user_runner(
        json!({"UserPromptSubmit": [{"hooks": [command_hook("printf 'submit denied' >&2; exit 2")]}]}),
        temp.path(),
    );
    let outcome = runner
        .run_event(
            &HookInput::new(
                HookEvent::UserPromptSubmit,
                "s",
                temp.path().to_string_lossy(),
            )
            .with_prompt("hello"),
        )
        .await
        .unwrap();
    assert_eq!(outcome.decision, HookDecision::Deny);
    assert_eq!(outcome.blocking_error(), Some("submit denied"));

    // PreCompact：exit 2 阻止压缩。
    let runner = user_runner(
        json!({"PreCompact": [{"hooks": [command_hook("printf 'compact denied' >&2; exit 2")]}]}),
        temp.path(),
    );
    let outcome = runner
        .run_event(
            &HookInput::new(HookEvent::PreCompact, "s", temp.path().to_string_lossy())
                .with_trigger("manual"),
        )
        .await
        .unwrap();
    assert_eq!(outcome.decision, HookDecision::Deny);
    assert_eq!(outcome.blocking_error(), Some("compact denied"));
}

#[tokio::test]
async fn invalid_json_output_only_warns() {
    let temp = TempDir::new().unwrap();
    let runner = user_runner(
        json!({"PreToolUse": [{"hooks": [command_hook("printf '%s' '{\"continue\": tru'")]}]}),
        temp.path(),
    );
    let outcome = runner
        .run_event(&pre_tool_input(temp.path(), "bash", json!({})))
        .await
        .unwrap();
    assert_eq!(outcome.decision, HookDecision::Passthrough);
    assert_eq!(outcome.runs[0].outcome, HookAuditOutcome::InvalidOutput);
    assert!(!outcome.warnings.is_empty());
}

#[tokio::test]
async fn stdin_carries_event_specific_fields() {
    let temp = TempDir::new().unwrap();
    let stdin_file = temp.path().join("post-tool.json");

    let mut registry = HookRegistry::new();
    add_source(
        &mut registry,
        HookSource::User,
        json!({"PostToolUse": [{"hooks": [
            command_hook(&format!("cat > {}", stdin_file.display()))
        ]}]}),
        true,
    );
    let runner = build_runner(registry, temp.path());

    let input = HookInput::new(
        HookEvent::PostToolUse,
        "session-fields",
        temp.path().to_string_lossy(),
    )
    .with_transcript_path("/tmp/transcript.jsonl")
    .with_permission_mode("accept-edits")
    .with_tool_call("write", json!({"path": "a.txt"}))
    .with_tool_response(json!({"isError": false, "content": "ok"}));
    runner.run_event(&input).await.unwrap();

    let stdin: Value =
        serde_json::from_str(&std::fs::read_to_string(&stdin_file).unwrap()).unwrap();
    assert_eq!(stdin["session_id"], "session-fields");
    assert_eq!(stdin["transcript_path"], "/tmp/transcript.jsonl");
    assert_eq!(stdin["permission_mode"], "accept-edits");
    assert_eq!(stdin["hook_event_name"], "PostToolUse");
    assert_eq!(stdin["tool_name"], "write");
    assert_eq!(stdin["tool_input"], json!({"path": "a.txt"}));
    assert_eq!(
        stdin["tool_response"],
        json!({"isError": false, "content": "ok"})
    );

    // PostToolUseFailure 的 error 字段。
    let stdin_file = temp.path().join("post-failure.json");
    let mut registry = HookRegistry::new();
    add_source(
        &mut registry,
        HookSource::User,
        json!({"PostToolUseFailure": [{"hooks": [
            command_hook(&format!("cat > {}", stdin_file.display()))
        ]}]}),
        true,
    );
    let runner = build_runner(registry, temp.path());
    let input = HookInput::new(
        HookEvent::PostToolUseFailure,
        "session-fields",
        temp.path().to_string_lossy(),
    )
    .with_tool_call("bash", json!({"command": "false"}))
    .with_error("exit status 1");
    runner.run_event(&input).await.unwrap();
    let stdin: Value =
        serde_json::from_str(&std::fs::read_to_string(&stdin_file).unwrap()).unwrap();
    assert_eq!(stdin["error"], "exit status 1");
    assert_eq!(stdin["tool_name"], "bash");
}

#[tokio::test]
async fn missing_tool_name_is_a_typed_input_error() {
    let temp = TempDir::new().unwrap();
    let runner = user_runner(json!({}), temp.path());
    let error = runner
        .run_event(&HookInput::new(
            HookEvent::PreToolUse,
            "s",
            temp.path().to_string_lossy(),
        ))
        .await
        .expect_err("tool_name is required");
    assert!(matches!(
        error,
        kiana_hooks::HookError::InvalidInput(message) if message.contains("tool_name")
    ));
}
