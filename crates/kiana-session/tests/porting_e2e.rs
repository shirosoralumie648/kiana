//! 「按周并行移植」跨模块端到端验收。
//!
//! 这个文件放在 `kiana-session` 而不是 `kiana-coding-agent`，原因是验收项需要同时
//! 访问 `kiana-sandbox`（真实 bwrap / 统一执行策略）、`kiana-hooks`、`kiana-memory`
//! 与 `AgentSession` 的测试夹具（`tests/common`）。`kiana-coding-agent` 只是这些 crate
//! 的下游消费者，没有这些直接依赖，也没有会话夹具；把测试放进 coding-agent 需要新增
//! 三个 dev-dependency 并复制整套 JsEngine/faux-provider 夹具，属于为测试扩大依赖面。
//! `kiana-session` 已经是这些模块的接线层，测试使用它的真实公开 API，不 mock 核心路径。
//!
//! 覆盖：
//! - 权限：read-only 拒绝 `execute_bash`、default 无 UI fail-closed、bypass 放行；
//!   AllowOnce 不持久、AllowAlways{Session} 在会话内持久且不落盘；
//! - 沙箱：workspace-write 的文件工具工作区外写入被拒；shell 在真实 bwrap 下工作区外
//!   写入失败（bwrap 不可用时断言 `BackendUnavailable` 且命令没有执行）；非法策略的
//!   fail-closed manager 在子进程中验证「命令没有执行」；
//! - 读后写：没读就 edit 被拒；读后外部修改返回 `FileChangedSinceRead`；
//! - hooks：PreToolUse exit 2 阻止工具；updatedInput 改写后权限层看到新参数；
//! - 压缩：pruning 后 tool call/result 配对合法；落盘 artifact 可以按 offset 回读；
//! - memory：写入 → 召回 → forget，secret 不落盘（索引/主题/审计/墓碑全部扫描）；
//! - 子代理：父权限运行期收窄后，子代理不能调用未授权工具，只读工具仍可用。

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use common::{
    HarnessOptions, ScriptedUi, create_harness, faux_text_response, get_message_text,
    get_tool_result, run, settings_from_json,
};
use indexmap::IndexMap;
use kiana_agent_core::types::AgentMessage;
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message, faux_tool_call,
};
use kiana_ai::types::{Message, StopReason};
use kiana_compaction::pruning::{
    PruneConfig, enforce_aggregate_tool_result_budget, prune_tool_outputs, tool_pairing_violations,
};
use kiana_config::settings::permissions::{local_settings_path, read_permissions_file};
use kiana_config::settings::schema::Settings;
use kiana_core::artifact_store::ArtifactStore;
use kiana_hooks::HookAuditOutcome;
use kiana_memory::{
    MemoryConfig, MemoryEntryDraft, MemoryScope, MemorySelector, MemoryService, MemoryStore,
    MemoryType, RecallCandidate, RecallConfig, RecallSession, ScopePaths, WriteBatch, WriteSource,
    read_tombstones,
};
use kiana_permissions::{
    PermissionEngine, PermissionFlags, PermissionMode, PermissionRequest, PermissionRule,
    RuleDecision,
};
use kiana_sandbox::{
    ExecutionPolicy, ExecutionStatus, SandboxError, SandboxManager, SandboxPolicy, SandboxType,
    SpawnOptions, StdioMode,
};
use kiana_session::agent_definition::builtin_agent_definitions;
use kiana_session::agent_session::{ExecuteBashOptions, ExtensionBindings, PromptOptions};
use kiana_session::subagent::{SubagentApprovalMode, SubagentError, SubagentRuntimeOptions};
use kiana_session_core::session_manager::{SessionEntry, SessionMessageEntry, SessionMessageType};
use kiana_tools::read::ReadStateRegistry;
use kiana_tools::write::{WriteToolOptions, create_write_tool};
use serde_json::{Value, json};

// ===========================================================================
// 权限
// ===========================================================================

fn settings_with_permissions(permissions: Value) -> Settings {
    settings_from_json(json!({ "permissions": permissions }))
}

fn settings_with_hooks(hooks: Value, permissions: Value) -> Settings {
    settings_from_json(json!({ "hooks": hooks, "permissions": permissions }))
}

/// read-only 拒绝 `execute_bash`；default 在没有交互 UI 时 fail-closed；bypass 放行
/// 且命令真的执行。三条都走 `AgentSession::execute_bash` 的真实入口。
#[tokio::test]
async fn permission_modes_are_enforced_on_the_real_execute_bash_path() {
    run(async {
        let read_only = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({"mode": "read-only"}))),
            ..Default::default()
        })
        .await;
        assert_eq!(
            read_only.session.permission_mode(),
            Some(PermissionMode::ReadOnly)
        );
        let error = read_only
            .session
            .execute_bash("printf should-not-run".to_string(), None, None)
            .await
            .expect_err("read-only must reject execute_bash");
        assert!(
            error.to_string().contains("read-only"),
            "unexpected read-only error: {error}"
        );
        read_only.cleanup();

        let default = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({"mode": "default"}))),
            ..Default::default()
        })
        .await;
        let error = default
            .session
            .execute_bash("printf should-not-run".to_string(), None, None)
            .await
            .expect_err("default mode must fail closed without a UI");
        assert!(
            error.to_string().contains("No interactive UI"),
            "unexpected fail-closed error: {error}"
        );
        default.cleanup();

        let bypass = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({"mode": "bypass"}))),
            ..Default::default()
        })
        .await;
        let result = bypass
            .session
            .execute_bash(
                "printf e2e-bypass".to_string(),
                None,
                Some(ExecuteBashOptions::default()),
            )
            .await
            .expect("bypass must allow execute_bash");
        assert!(
            result.output.contains("e2e-bypass"),
            "unexpected output: {}",
            result.output
        );
        bypass.cleanup();
    })
    .await;
}

/// AllowOnce 只放行本次，不写任何持久化文件；AllowAlways{Session} 在会话内生效，
/// 但同样不落盘（模拟重启后仍会重新询问）。
#[tokio::test]
async fn allow_once_does_not_persist_and_session_scope_only_lasts_for_the_session() {
    run(async {
        // --- AllowOnce：第二次同样的调用必须再次询问，并可以拒绝 ---
        let once = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({"mode": "default"}))),
            ..Default::default()
        })
        .await;
        let ui = ScriptedUi::new(
            vec![Some("Allow once".to_string()), Some("Reject".to_string())],
            vec![Some("not this time".to_string())],
        );
        once.session
            .bind_extensions(ExtensionBindings {
                ui_context: Some(ui.clone()),
                ..Default::default()
            })
            .await;
        once.session
            .execute_bash("printf allow-once".to_string(), None, None)
            .await
            .expect("the first AllowOnce must run");
        let error = once
            .session
            .execute_bash("printf allow-once".to_string(), None, None)
            .await
            .expect_err("AllowOnce must not cover the next identical call");
        assert!(
            error.to_string().contains("not this time"),
            "reject reason must reach the caller: {error}"
        );
        assert_eq!(ui.select_count(), 2, "only the first call was approved");
        let once_path = local_settings_path(once.temp_dir.path());
        assert!(
            read_permissions_file(&once_path)
                .expect("read local settings")
                .is_none(),
            "AllowOnce must not write local settings"
        );
        let engine = PermissionEngine::from_settings_and_flags(
            once.temp_dir.path(),
            Some(&json!({"mode": "default"})),
            &PermissionFlags::default(),
        )
        .expect("engine from settings");
        let request = PermissionRequest::from_tool_call(
            "bash",
            &json!({"command": "printf allow-once"}),
            once.temp_dir.path(),
        );
        assert!(
            engine.evaluate(&request).is_ask(),
            "after a simulated restart the command must ask again"
        );
        once.cleanup();

        // --- AllowAlways{Session}：会话内第二次不再询问，但不写磁盘 ---
        let session = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({"mode": "default"}))),
            ..Default::default()
        })
        .await;
        let ui = ScriptedUi::new(
            vec![
                Some("Always allow".to_string()),
                Some("This session".to_string()),
            ],
            Vec::new(),
        );
        session
            .session
            .bind_extensions(ExtensionBindings {
                ui_context: Some(ui.clone()),
                ..Default::default()
            })
            .await;
        session
            .session
            .execute_bash("printf session-scope".to_string(), None, None)
            .await
            .expect("first session-scope approval runs the command");
        session
            .session
            .execute_bash("printf session-scope".to_string(), None, None)
            .await
            .expect("the session-scoped allowance must cover the second call");
        assert_eq!(
            ui.select_count(),
            2,
            "only the first call opens choice + scope selectors"
        );
        let session_path = local_settings_path(session.temp_dir.path());
        assert!(
            read_permissions_file(&session_path)
                .expect("read local settings")
                .is_none(),
            "session scope must not persist to local settings"
        );
        let engine = PermissionEngine::from_settings_and_flags(
            session.temp_dir.path(),
            Some(&json!({"mode": "default"})),
            &PermissionFlags::default(),
        )
        .expect("engine from settings");
        let request = PermissionRequest::from_tool_call(
            "bash",
            &json!({"command": "printf session-scope"}),
            session.temp_dir.path(),
        );
        assert!(
            engine.evaluate(&request).is_ask(),
            "session scope must not survive a restart"
        );
        session.cleanup();
    })
    .await;
}

// ===========================================================================
// 沙箱
// ===========================================================================

fn sandbox_scratch_base() -> PathBuf {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(&base).expect("create target tmp");
    base
}

fn sandbox_tempdir(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(sandbox_scratch_base())
        .expect("tempdir")
}

fn relay_options(cwd: &Path) -> SpawnOptions {
    SpawnOptions {
        cwd: Some(cwd.to_path_buf()),
        stdin: StdioMode::Ignore,
        stdout: StdioMode::Ignore,
        stderr: StdioMode::Ignore,
        detached: true,
        ..Default::default()
    }
}

/// workspace-write 下 `write` 工具不能写工作区外；拒绝发生在任何文件操作之前。
#[tokio::test]
async fn workspace_write_file_tool_rejects_writes_outside_the_workspace() {
    let workspace = sandbox_tempdir("kiana-e2e-ws-");
    let outside = sandbox_tempdir("kiana-e2e-outside-");
    let policy = Arc::new(ExecutionPolicy::new(
        SandboxPolicy::workspace_write(workspace.path()),
        workspace.path(),
    ));
    let tool = create_write_tool(
        workspace.path().to_string_lossy().into_owned(),
        Some(WriteToolOptions {
            execution_policy: Some(Arc::clone(&policy)),
            read_state: Some(Arc::new(ReadStateRegistry::new())),
            operations: None,
        }),
    );
    let target = outside.path().join("escaped.txt");
    let error = tool
        .execute
        .execute(
            "call-1",
            json!({"path": target.to_string_lossy(), "content": "escaped"}),
            None,
            None,
        )
        .await
        .expect_err("workspace-write must reject writes outside the workspace");
    assert!(
        error.message.contains("OUTSIDE_WORKSPACE"),
        "unexpected denial: {}",
        error.message
    );
    assert!(!target.exists(), "the denied write must not create a file");

    // 对照：同一个工具写工作区内成功，证明拒绝来自路径策略而不是工具本身坏了。
    let inside = workspace.path().join("inside.txt");
    tool.execute
        .execute(
            "call-2",
            json!({"path": inside.to_string_lossy(), "content": "inside"}),
            None,
            None,
        )
        .await
        .expect("workspace write must succeed");
    assert_eq!(std::fs::read_to_string(&inside).expect("read"), "inside");
}

/// 真实 bwrap：workspace-write 下 shell 写工作区外失败、写工作区内成功。
/// bwrap 不可用的宿主上，显式选择 bwrap 必须返回 `Unavailable`，映射到
/// `BackendUnavailable`，且命令没有执行（不假装跑过）。
#[tokio::test]
async fn workspace_write_shell_uses_real_bubblewrap_or_reports_backend_unavailable() {
    let workspace = sandbox_tempdir("kiana-e2e-bwrap-ws-");
    let outside = sandbox_tempdir("kiana-e2e-bwrap-out-");
    let marker = outside.path().join("escaped.txt");
    let stderr_path = workspace.path().join("stderr.txt");
    let policy = SandboxPolicy::workspace_write(workspace.path());

    let manager =
        match SandboxManager::with_type(SandboxType::LinuxBubblewrap, policy, workspace.path()) {
            Ok(manager) => manager,
            Err(error) => {
                // 不在有 bwrap 的宿主上假装执行：只断言 fail-closed 契约。
                assert!(
                    matches!(
                        error,
                        SandboxError::Unavailable { .. } | SandboxError::Unsupported(_)
                    ),
                    "unexpected backend error: {error:?}"
                );
                let status = ExecutionStatus::from_sandbox_error(&error);
                assert!(
                    matches!(status, ExecutionStatus::BackendUnavailable { .. }),
                    "unavailable backend must map to BackendUnavailable: {status:?}"
                );
                assert!(
                    !status.may_have_executed(),
                    "a failed backend must not claim the command may have run"
                );
                assert!(!marker.exists(), "the command must not have run");
                eprintln!("real bwrap assertion skipped: {error}");
                return;
            }
        };
    assert_eq!(manager.sandbox_type(), SandboxType::LinuxBubblewrap);

    // 工作区外写入：命令在 bwrap 里执行，目标路径只读，写入必须失败。
    let outside_command = format!(
        "printf escaped 2>'{}' >'{}'",
        stderr_path.display(),
        marker.display()
    );
    let mut relayed = manager
        .spawn_relayed(
            "/bin/sh",
            &["-c".to_string(), outside_command],
            &relay_options(workspace.path()),
        )
        .expect("spawn outside-write probe");
    let status = relayed.child_mut().wait().await.expect("wait");
    let outcome = relayed.outcome();
    assert!(
        !status.success(),
        "writing outside the workspace must fail: {outcome:?}"
    );
    assert!(
        matches!(outcome, ExecutionStatus::ConfirmedExit { code, .. } if code != 0),
        "expected a confirmed non-zero exit, got {outcome:?}"
    );
    assert!(!marker.exists(), "bwrap must block the outside write");
    let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
    assert!(
        !stderr.trim().is_empty(),
        "the shell must report why the write failed"
    );

    // 工作区内写入：同一个 manager 下成功。
    let inside = workspace.path().join("inside.txt");
    let mut relayed = manager
        .spawn_relayed(
            "/bin/sh",
            &[
                "-c".to_string(),
                format!("printf inside >'{}'", inside.display()),
            ],
            &relay_options(workspace.path()),
        )
        .expect("spawn inside-write probe");
    let status = relayed.child_mut().wait().await.expect("wait");
    assert_eq!(status.code(), Some(0), "workspace write must succeed");
    assert_eq!(relayed.outcome().exit_code(), Some(0));
    assert_eq!(std::fs::read_to_string(&inside).expect("read"), "inside");
}

/// 子进程 worker：只在父测试设置的环境变量存在时执行。
///
/// `SandboxManager::from_env` 读取进程级环境变量；用重新执行当前测试二进制的方式隔离，
/// 避免污染同一测试进程里并行运行的其他会话测试。
#[test]
fn invalid_policy_sandbox_worker() {
    let Ok(workspace) = std::env::var("KIANA_E2E_SANDBOX_WORKSPACE") else {
        return;
    };
    let workspace = PathBuf::from(workspace);
    let marker = workspace.join("marker.txt");
    // 父测试用这一行确认 worker 真的被执行（而不是被过滤器跳过、0 个测试也返回成功）。
    println!("KIANA_E2E_INVALID_POLICY_WORKER_RAN");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("worker runtime");
    runtime.block_on(async {
        let manager = SandboxManager::from_env(&workspace)
            .expect("an invalid policy must produce a fail-closed manager");
        let error = manager
            .spawn_relayed(
                "/bin/sh",
                &[
                    "-c".to_string(),
                    format!("printf ran >'{}'", marker.display()),
                ],
                &relay_options(&workspace),
            )
            .expect_err("a fail-closed manager must reject spawn");
        assert!(
            matches!(error, SandboxError::Unavailable { .. }),
            "unexpected error: {error:?}"
        );
        assert!(!marker.exists(), "the command must not have run");
        let status = ExecutionStatus::from_sandbox_error(&error);
        assert!(
            matches!(status, ExecutionStatus::BackendUnavailable { .. }),
            "{status:?}"
        );
        assert!(!status.may_have_executed());
        assert!(status.user_message().contains("was not executed"));
    });
}

/// 非法 `KIANA_SANDBOX_POLICY`：manager 是 fail-closed 的，第一条命令以
/// `BackendUnavailable` 失败且没有任何副作用。在独立子进程里改环境变量。
#[test]
fn invalid_sandbox_policy_fails_closed_without_running_the_command() {
    let workspace = sandbox_tempdir("kiana-e2e-invalid-policy-");
    let executable = std::env::current_exe().expect("current test executable");
    let output = std::process::Command::new(executable)
        .arg("--exact")
        .arg("invalid_policy_sandbox_worker")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env("KIANA_E2E_SANDBOX_WORKSPACE", workspace.path())
        .env("KIANA_SANDBOX_POLICY", "not-a-real-policy")
        .env_remove("KIANA_SANDBOX")
        .output()
        .expect("spawn sandbox worker");
    assert!(
        output.status.success(),
        "sandbox worker failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("KIANA_E2E_INVALID_POLICY_WORKER_RAN"),
        "the worker test did not execute (check the --exact filter); stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        !workspace.path().join("marker.txt").exists(),
        "the fail-closed manager must not run the command"
    );
}

// ===========================================================================
// 读后写（read-before-write）
// ===========================================================================

fn tool_call_step(name: &str, arguments: IndexMap<String, Value>) -> FauxResponseStep {
    FauxResponseStep::Message(faux_assistant_message(
        faux_tool_call(name, arguments),
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        },
    ))
}

fn text_step(text: &str) -> FauxResponseStep {
    faux_text_response(text)
}

fn edit_arguments(path: &Path, old_text: &str, new_text: &str) -> IndexMap<String, Value> {
    let mut arguments = IndexMap::new();
    arguments.insert("path".to_string(), json!(path.to_string_lossy()));
    arguments.insert(
        "edits".to_string(),
        json!([{ "oldText": old_text, "newText": new_text }]),
    );
    arguments
}

fn read_arguments(path: &Path) -> IndexMap<String, Value> {
    let mut arguments = IndexMap::new();
    arguments.insert("path".to_string(), json!(path.to_string_lossy()));
    arguments
}

/// 走 `AgentSession` 的默认 `read`/`edit` 工具表：
/// 1. 没有读过文件就 edit → `ReadBeforeEdit`，文件不变；
/// 2. read 之后外部进程改写文件 → edit 返回 `FileChangedSinceRead`，文件仍保持外部内容。
#[tokio::test]
async fn edit_requires_a_fresh_read_and_detects_external_modification() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({"mode": "bypass"}))),
            ..Default::default()
        })
        .await;
        let file = harness.temp_dir.path().join("notes.txt");
        std::fs::write(&file, "hello\n").expect("fixture");

        // 1. 未读先改。
        harness.set_responses(vec![
            tool_call_step("edit", edit_arguments(&file, "hello", "world")),
            text_step("done"),
        ]);
        harness
            .session
            .prompt("edit the file".to_string(), PromptOptions::default())
            .await
            .expect("prompt");
        let result = get_tool_result(&harness, "edit");
        let text = get_message_text(&AgentMessage::Standard(Message::ToolResult(result.clone())));
        assert!(result.is_error, "unread edit must fail: {text}");
        assert!(
            text.contains("ReadBeforeEdit"),
            "unexpected error text: {text}"
        );
        assert_eq!(
            std::fs::read_to_string(&file).expect("read"),
            "hello\n",
            "a rejected edit must not modify the file"
        );

        // 2. 读一次，随后外部进程改写。
        harness.set_responses(vec![
            tool_call_step("read", read_arguments(&file)),
            text_step("read done"),
        ]);
        harness
            .session
            .prompt("read the file".to_string(), PromptOptions::default())
            .await
            .expect("prompt");
        let result = get_tool_result(&harness, "read");
        assert!(!result.is_error, "read must succeed");
        assert!(
            get_message_text(&AgentMessage::Standard(Message::ToolResult(result)))
                .contains("hello"),
            "read must return the file"
        );

        std::fs::write(&file, "hello changed elsewhere\n").expect("external write");
        harness.set_responses(vec![
            tool_call_step("edit", edit_arguments(&file, "hello", "world")),
            text_step("done"),
        ]);
        harness
            .session
            .prompt("edit it again".to_string(), PromptOptions::default())
            .await
            .expect("prompt");
        let result = get_tool_result(&harness, "edit");
        let text = get_message_text(&AgentMessage::Standard(Message::ToolResult(result.clone())));
        assert!(result.is_error, "stale read must fail: {text}");
        assert!(
            text.contains("FileChangedSinceRead"),
            "unexpected error text: {text}"
        );
        assert!(
            text.contains("FILE_CHANGED_SINCE_READ"),
            "the stable error code must be present: {text}"
        );
        assert_eq!(
            std::fs::read_to_string(&file).expect("read"),
            "hello changed elsewhere\n",
            "a stale edit must not overwrite the external change"
        );
        harness.cleanup();
    })
    .await;
}

// ===========================================================================
// Hooks
// ===========================================================================

/// PreToolUse exit 2：工具不执行，stderr 作为模型可见错误出现一次，审计记为 blocked。
#[tokio::test]
async fn pre_tool_use_exit_two_blocks_the_tool_before_execution() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_with_hooks(
                json!({
                    "PreToolUse": [{
                        "matcher": "bash",
                        "hooks": [{"type": "command", "command": "sh .hook-deny.sh"}]
                    }]
                }),
                json!({"mode": "bypass"}),
            )),
            ..Default::default()
        })
        .await;
        std::fs::write(
            harness.temp_dir.path().join(".hook-deny.sh"),
            "#!/bin/sh\ncat >/dev/null\necho 'blocked by e2e hook' >&2\nexit 2\n",
        )
        .expect("write hook");
        harness.set_responses(vec![
            tool_call_step("bash", {
                let mut args = IndexMap::new();
                args.insert("command".to_string(), json!("touch .hook-should-not-exist"));
                args
            }),
            text_step("done"),
        ]);
        harness
            .session
            .prompt("run it".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        let result = get_tool_result(&harness, "bash");
        let text = get_message_text(&AgentMessage::Standard(Message::ToolResult(result.clone())));
        assert!(result.is_error, "hook exit 2 must fail the tool: {text}");
        assert_eq!(
            text.matches("blocked by e2e hook").count(),
            1,
            "blocking stderr must appear exactly once: {text}"
        );
        assert!(
            !harness
                .temp_dir
                .path()
                .join(".hook-should-not-exist")
                .exists(),
            "a blocked tool must not execute"
        );
        let audit = harness.session.hook_audit_records();
        assert!(
            audit.iter().any(|record| {
                record.event == kiana_hooks::HookEvent::PreToolUse
                    && record.outcome == HookAuditOutcome::Blocked
            }),
            "blocked hook must be audited: {audit:?}"
        );
        harness.cleanup();
    })
    .await;
}

/// PreToolUse `updatedInput` 在权限判断之前整体替换参数：原命令 `printf original`
/// 不在 deny 规则里，改写后的 `printf denied` 才命中规则。
#[tokio::test]
async fn pre_tool_use_updated_input_is_seen_by_the_permission_layer() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_with_hooks(
                json!({
                    "PreToolUse": [{
                        "matcher": "bash",
                        "hooks": [{"type": "command", "command": "sh .hook-rewrite.sh"}]
                    }]
                }),
                json!({
                    "mode": "bypass",
                    "rules": [{
                        "command": ["printf", "denied"],
                        "decision": "deny",
                        "reason": "denied after hook rewrite"
                    }]
                }),
            )),
            ..Default::default()
        })
        .await;
        std::fs::write(
            harness.temp_dir.path().join(".hook-rewrite.sh"),
            "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"updatedInput\":{\"command\":\"printf denied\"}}}'\n",
        )
        .expect("write hook");
        harness.set_responses(vec![
            tool_call_step("bash", {
                let mut args = IndexMap::new();
                args.insert("command".to_string(), json!("printf original"));
                args
            }),
            text_step("done"),
        ]);
        harness
            .session
            .prompt("run it".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        let result = get_tool_result(&harness, "bash");
        let text = get_message_text(&AgentMessage::Standard(Message::ToolResult(result.clone())));
        assert!(
            result.is_error,
            "the rewritten command must be denied: {text}"
        );
        assert!(
            text.contains("denied after hook rewrite"),
            "permission layer must evaluate the updatedInput, not the original: {text}"
        );
        harness.cleanup();
    })
    .await;
}

// ===========================================================================
// 压缩：pruning 配对 + artifact offset 回读
// ===========================================================================

fn session_entry(factory: &mut SessionEntryFactory, message: AgentMessage) -> SessionEntry {
    factory.entry(message)
}

struct SessionEntryFactory {
    next: usize,
    last: Option<String>,
}

impl SessionEntryFactory {
    fn new() -> Self {
        Self {
            next: 0,
            last: None,
        }
    }

    fn entry(&mut self, message: AgentMessage) -> SessionEntry {
        let id = format!("e2e-entry-{}", self.next);
        self.next += 1;
        let entry = SessionEntry::Message(SessionMessageEntry {
            kind: SessionMessageType::Marker,
            id: id.clone(),
            parent_id: self.last.clone(),
            timestamp: String::new(),
            message,
            raw_message: None,
        });
        self.last = Some(id);
        entry
    }
}

fn assistant_tool_call(id: &str, name: &str, arguments: Value) -> AgentMessage {
    let arguments = arguments
        .as_object()
        .map(|object| object.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    AgentMessage::Standard(Message::Assistant(kiana_ai::types::AssistantMessage {
        role: kiana_ai::types::AssistantRole::Assistant,
        content: vec![kiana_ai::types::AssistantContentBlock::ToolCall(
            kiana_ai::types::ToolCall {
                kind_tool_call: kiana_ai::types::ToolCallKind::default(),
                id: id.to_string(),
                name: name.to_string(),
                arguments,
                thought_signature: None,
                namespace: None,
                arguments_error: None,
            },
        )],
        usage: Default::default(),
        stop_reason: kiana_ai::types::StopReason::Stop,
        timestamp: 0,
        ..Default::default()
    }))
}

fn tool_result_message(name: &str, id: &str, text: &str) -> AgentMessage {
    AgentMessage::Standard(Message::ToolResult(kiana_ai::types::ToolResultMessage {
        role: Default::default(),
        tool_call_id: id.to_string(),
        tool_name: name.to_string(),
        content: vec![kiana_ai::types::UserContentBlock::Text(
            kiana_ai::types::TextContent {
                kind_text: kiana_ai::types::TextKind::Text,
                text: text.to_string(),
                text_signature: None,
            },
        )],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 0,
        duration_ms: None,
    }))
}

/// 从一个 `SessionEntry` 里取出 tool result 的纯文本（测试断言用）。
fn entry_result_text(entry: &SessionEntry) -> String {
    let SessionEntry::Message(message_entry) = entry else {
        return String::new();
    };
    let AgentMessage::Standard(Message::ToolResult(result)) = &message_entry.message else {
        return String::new();
    };
    result
        .content
        .iter()
        .filter_map(|block| match block {
            kiana_ai::types::UserContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn call_and_result_ids(entries: &[SessionEntry]) -> (Vec<String>, Vec<String>) {
    let mut calls = Vec::new();
    let mut results = Vec::new();
    for entry in entries {
        let SessionEntry::Message(message_entry) = entry else {
            continue;
        };
        match &message_entry.message {
            AgentMessage::Standard(Message::Assistant(assistant)) => {
                for block in &assistant.content {
                    if let kiana_ai::types::AssistantContentBlock::ToolCall(call) = block {
                        calls.push(call.id.clone());
                    }
                }
            }
            AgentMessage::Standard(Message::ToolResult(result)) => {
                results.push(result.tool_call_id.clone());
            }
            _ => {}
        }
    }
    (calls, results)
}

/// 压缩前 pruning 只改写 tool result 内容：entry 顺序/id 不变，tool call 与 result
/// 一一配对；同时验证聚合预算落盘的 artifact 可以按 offset 逐字节回读。
#[test]
fn pruning_keeps_tool_pairs_valid_and_artifacts_are_offset_readable() {
    let mut factory = SessionEntryFactory::new();
    let payload = "x".repeat(30_000);
    let mut entries = Vec::new();
    for index in 0..20 {
        let call_id = format!("call-{index}");
        entries.push(session_entry(
            &mut factory,
            assistant_tool_call(&call_id, "read", json!({"path": format!("/file{index}")})),
        ));
        entries.push(session_entry(
            &mut factory,
            tool_result_message("read", &call_id, &payload),
        ));
    }
    let before_ids: Vec<String> = entries.iter().map(|entry| entry.id().to_string()).collect();

    let outcome = prune_tool_outputs(&entries, &PruneConfig::default());
    assert!(outcome.applied, "expected pruning to reclaim tokens");
    assert!(
        outcome.tokens_saved >= 20_000,
        "saved {}",
        outcome.tokens_saved
    );
    let after_ids: Vec<String> = outcome
        .entries
        .iter()
        .map(|entry| entry.id().to_string())
        .collect();
    assert_eq!(
        before_ids, after_ids,
        "pruning must not reorder or drop entries"
    );
    assert!(
        tool_pairing_violations(&outcome.entries).is_empty(),
        "pruned entries must keep valid tool pairs"
    );
    let (calls, results) = call_and_result_ids(&outcome.entries);
    assert_eq!(calls.len(), 20);
    assert_eq!(results, calls, "every result keeps its original call id");
    // 旧结果确实被换成了 pruning 的精确占位符，而最新结果保持完整。
    let oldest_text = entry_result_text(&outcome.entries[1]);
    assert!(
        oldest_text.starts_with("[Output truncated"),
        "the oldest result must carry the pruning placeholder, got: {oldest_text:?}"
    );
    assert_eq!(
        entry_result_text(&outcome.entries[outcome.entries.len() - 1]),
        payload,
        "the newest result must stay intact"
    );

    // Artifact：聚合预算把完整内容落盘，占位符带路径；按 offset 回读必须逐字节一致。
    let artifact_root = sandbox_tempdir("kiana-e2e-artifacts-");
    let store = ArtifactStore::new(artifact_root.path(), "e2e-session").expect("artifact store");
    let content: String = (0..2_000)
        .map(|index| format!("line {index:04}\n"))
        .collect();
    let mut factory = SessionEntryFactory::new();
    let budget_entries = vec![
        session_entry(
            &mut factory,
            assistant_tool_call("artifact-call", "bash", json!({"command": "echo big"})),
        ),
        session_entry(
            &mut factory,
            tool_result_message("bash", "artifact-call", &content),
        ),
        // 最新结果永远保留：预算再紧也不能把模型刚拿到的输出换成占位符。
        session_entry(
            &mut factory,
            assistant_tool_call("newest-call", "bash", json!({"command": "echo newest"})),
        ),
        session_entry(
            &mut factory,
            tool_result_message("bash", "newest-call", "newest output"),
        ),
    ];
    let budget = enforce_aggregate_tool_result_budget(
        &budget_entries,
        1,
        Some(&store),
        &PruneConfig::default(),
    );
    assert_eq!(budget.cleared.len(), 1, "{budget:?}");
    assert_eq!(
        budget.cleared[0].entry_id,
        budget_entries[1].id(),
        "the oldest result must be cleared first"
    );
    assert_eq!(
        entry_result_text(&budget.entries[3]),
        "newest output",
        "the newest result must stay intact"
    );
    assert_eq!(budget.cleared_without_artifact, 0);
    let notice = &budget.cleared[0].notice;
    let path = notice
        .strip_prefix("[Old tool result content cleared; artifact: ")
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or_else(|| panic!("unexpected artifact notice: {notice}"));
    assert!(
        Path::new(path).exists(),
        "the artifact file must survive pruning: {path}"
    );
    let id = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.split('.').next())
        .expect("artifact id");
    let (offset, length) = (1_234u64, 321u64);
    let expected = &content[offset as usize..(offset + length) as usize];
    assert_eq!(
        store
            .read_range(id, offset, length)
            .expect("offset readback"),
        expected,
        "artifact offset readback must match the original bytes"
    );
    // 占位符替换后 tool pair 仍然合法。
    assert!(tool_pairing_violations(&budget.entries).is_empty());
}

// ===========================================================================
// Memory：写入 → 召回 → forget，secret 不落盘
// ===========================================================================

struct FixedMemorySelector {
    ids: Vec<String>,
}

#[async_trait]
impl MemorySelector for FixedMemorySelector {
    async fn select(
        &self,
        _query: &str,
        _candidates: &[RecallCandidate],
    ) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.ids.clone())
    }
}

fn scan_files(root: &Path, visits: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_files(&path, visits);
        } else if path.is_file() {
            visits.push(path);
        }
    }
}

fn assert_tree_does_not_contain(root: &Path, needle: &str) {
    let mut files = Vec::new();
    scan_files(root, &mut files);
    assert!(!files.is_empty(), "memory scope should contain files");
    for file in files {
        let bytes = std::fs::read(&file).expect("read memory file");
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains(needle),
            "secret {needle:?} leaked into {}",
            file.display()
        );
    }
}

/// 真实 `MemoryService` + `MemoryStore` + `RecallSession`：
/// 写入含 secret 的条目（落盘前脱敏）→ 召回注入 `<memory-context>` → forget 删除并写
/// 正文脱敏墓碑。索引/主题/审计/墓碑/状态文件全部扫描，secret 不得出现。
#[tokio::test]
async fn memory_write_recall_forget_keeps_secrets_off_disk() {
    let root_dir = tempfile::tempdir().expect("memory root");
    let root = root_dir.path().join("memory");
    let store = MemoryStore::open(
        ScopePaths::at_root(MemoryScope::Project, &root),
        MemoryConfig::default(),
    )
    .expect("open store");
    let service = MemoryService::trusted(store.clone());

    let openai_secret = "sk-abcdefghijklmnopqrstuvwxyz0123456789";
    let password = "hunter2-should-not-persist";
    let body = format!(
        "Rule: never deploy on Fridays.\nWhy: weekend coverage is thin.\nHow to apply: block deploys.\n\
         credentials: password={password}\napi_key={openai_secret}"
    );
    let report = service
        .apply(
            WriteBatch::new().add(MemoryEntryDraft::new(
                "deploy-rule",
                "never deploy on fridays",
                MemoryType::User,
                body,
            )),
            WriteSource::Tool,
        )
        .expect("write memory");
    assert!(
        report.redactions >= 2,
        "expected secret redaction before writing, got {}",
        report.redactions
    );
    let id = kiana_memory::slugify("deploy-rule");
    let entry = store.read_entry(&id).expect("stored entry");
    assert!(
        entry.body.contains("[REDACTED"),
        "stored body must be redacted: {}",
        entry.body
    );

    // 写入后扫描整个 scope：任何文件都不能包含 secret。
    assert_tree_does_not_contain(&root, password);
    assert_tree_does_not_contain(&root, openai_secret);

    // 召回：选择器只返回这一条，注入消息必须来自脱敏后的磁盘内容。
    let mut session = RecallSession::new();
    let selector = FixedMemorySelector {
        ids: vec![id.clone()],
    };
    let recalled = session
        .recall(
            &store,
            "deploy on friday?",
            Some(&selector),
            &RecallConfig {
                max_selected: 5,
                token_budget: 2_000,
                provider_timeout: std::time::Duration::from_millis(400),
            },
        )
        .await
        .expect("recall");
    let message = recalled.message.expect("recall must inject a message");
    assert!(message.content.contains("<memory-context>"));
    assert!(message.content.contains("deploy-rule"));
    assert!(message.content.contains("[REDACTED"));
    assert!(!message.content.contains(password));
    assert!(!message.content.contains(openai_secret));

    // forget：删除条目、写墓碑；墓碑只存 digest，不能带正文。
    let forgotten = service
        .forget(&id, "user asked to forget", 42, WriteSource::User)
        .expect("forget");
    assert_eq!(forgotten.removed.len(), 1);
    assert!(store.list_entries().expect("entries").is_empty());
    let tombstones = read_tombstones(&store.paths().tombstone_path).expect("tombstones");
    assert_eq!(tombstones.len(), 1);
    assert!(tombstones[0].body_digest.len() >= 16);
    assert_tree_does_not_contain(&root, password);
    assert_tree_does_not_contain(&root, openai_secret);

    // forget 之后重新召回：没有候选，不产生消息。
    let mut after = RecallSession::new();
    let recalled = after
        .recall(
            &store,
            "deploy on friday?",
            Some(&FixedMemorySelector { ids: vec![id] }),
            &RecallConfig {
                max_selected: 5,
                token_budget: 2_000,
                provider_timeout: std::time::Duration::from_millis(400),
            },
        )
        .await
        .expect("recall after forget");
    assert!(
        recalled.message.is_none(),
        "forgotten memory must not be recalled"
    );
}

// ===========================================================================
// 子代理：父权限收窄
// ===========================================================================

/// 父会话在运行期切换到 read-only 后，新建子代理继承收窄后的策略：bash/write 被拒，
/// read 仍可用；切回 bypass 并追加 deny 规则后，新子代理同样不能调用 bash。
#[tokio::test]
async fn narrowed_parent_permissions_are_enforced_for_subagents() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({"mode": "bypass"}))),
            ..Default::default()
        })
        .await;
        let file = harness.temp_dir.path().join("child-read.txt");
        std::fs::write(&file, "readable by child\n").expect("fixture");

        // 运行期收窄父权限。
        harness
            .session
            .set_permission_mode(PermissionMode::ReadOnly);
        let definition = builtin_agent_definitions()
            .into_iter()
            .find(|definition| definition.name == "general-purpose")
            .expect("general-purpose builtin");
        let runtime = harness
            .session
            .prepare_subagent(
                definition,
                SubagentRuntimeOptions {
                    approval: SubagentApprovalMode::Headless,
                    ..Default::default()
                },
            )
            .expect("prepare subagent");
        assert!(
            runtime.tool_names().iter().any(|name| name == "bash"),
            "bash is still in the parent tool pool; the permission layer must reject it"
        );
        let error = runtime
            .dispatch_tool("child-call-1", "bash", json!({"command": "printf nope"}))
            .await
            .expect_err("read-only parent must block child bash");
        match error {
            SubagentError::PermissionDenied { reason, tool } => {
                assert_eq!(tool, "bash");
                assert!(
                    reason.contains("read-only"),
                    "unexpected subagent denial: {reason}"
                );
            }
            other => panic!("unexpected subagent error: {other:?}"),
        }
        let error = runtime
            .dispatch_tool(
                "child-call-2",
                "write",
                json!({"path": file.to_string_lossy(), "content": "nope"}),
            )
            .await
            .expect_err("read-only parent must block child write");
        assert!(matches!(error, SubagentError::PermissionDenied { .. }));
        let allowed = runtime
            .dispatch_tool(
                "child-call-3",
                "read",
                json!({"path": file.to_string_lossy()}),
            )
            .await
            .expect("read-only child may still read");
        let text = allowed
            .content
            .iter()
            .filter_map(|block| match block {
                kiana_ai::types::UserContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("readable by child"),
            "child read result: {text}"
        );

        // 切回 bypass 并追加父 deny 规则：新子代理必须继承规则。
        harness
            .session
            .set_permission_mode(PermissionMode::BypassPermissions);
        harness
            .session
            .add_permission_rules([PermissionRule::tool("bash", RuleDecision::Deny)
                .with_reason("parent policy denies bash")]);
        let definition = builtin_agent_definitions()
            .into_iter()
            .find(|definition| definition.name == "general-purpose")
            .expect("general-purpose builtin");
        let runtime = harness
            .session
            .prepare_subagent(
                definition,
                SubagentRuntimeOptions {
                    approval: SubagentApprovalMode::Headless,
                    ..Default::default()
                },
            )
            .expect("prepare subagent");
        let error = runtime
            .dispatch_tool("child-call-4", "bash", json!({"command": "printf nope"}))
            .await
            .expect_err("parent deny rule must block child bash");
        match error {
            SubagentError::PermissionDenied { reason, .. } => {
                assert!(
                    reason.contains("parent policy denies bash"),
                    "unexpected subagent denial: {reason}"
                );
            }
            other => panic!("unexpected subagent error: {other:?}"),
        }
        harness.cleanup();
    })
    .await;
}
