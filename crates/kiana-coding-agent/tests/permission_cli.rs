//! CLI 权限 flag 的行为测试（第 1 周）。
//!
//! `--permission-mode` / `--allow` / `--deny` / `--add-dir` 的解析结果直接决定
//! 启动时交给 `PermissionEngine` 的 flags。这里把「解析 → 构造引擎 → 评估请求」串起来，
//! 锁定三件事：
//!
//! - CLI 模式覆盖 settings；
//! - `--deny` 永远优先于 `--allow`；
//! - `--add-dir` 扩大工作区，但 headless 下 `Ask` 仍是 `Ask`（调用方必须 fail-closed，
//!   不能静默允许）。

use kiana_coding_agent::cli::args::parse_args;
use kiana_permissions::{
    PermissionDecision, PermissionEngine, PermissionFlags, PermissionMode, PermissionRequest,
    RuleDecision,
};
use serde_json::json;

fn parse(args: &[&str]) -> kiana_coding_agent::cli::args::Args {
    let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    parse_args(&args)
}

fn flags_from_args(args: &[&str]) -> PermissionFlags {
    let parsed = parse(args);
    PermissionFlags {
        mode: parsed.permission_mode,
        allow: parsed.permission_allow,
        deny: parsed.permission_deny,
        add_dirs: parsed
            .add_dirs
            .into_iter()
            .map(std::path::PathBuf::from)
            .collect(),
    }
}

#[test]
fn parses_all_permission_flags() {
    let flags = flags_from_args(&[
        "--permission-mode",
        "accept-edits",
        "--allow",
        "tool:read",
        "--allow",
        "command:git status",
        "--deny",
        "tool:mcp__*",
        "--add-dir",
        "/tmp/extra",
    ]);
    assert_eq!(flags.mode, Some(PermissionMode::AcceptEdits));
    assert_eq!(flags.allow, vec!["tool:read", "command:git status"]);
    assert_eq!(flags.deny, vec!["tool:mcp__*"]);
    assert_eq!(flags.add_dirs, vec![std::path::PathBuf::from("/tmp/extra")]);
}

#[test]
fn cli_mode_overrides_settings() {
    let cwd = std::env::temp_dir();
    let settings = json!({"mode": "read-only"});
    let flags = flags_from_args(&["--permission-mode", "bypass"]);
    let engine =
        PermissionEngine::from_settings_and_flags(&cwd, Some(&settings), &flags).expect("engine");
    let request = PermissionRequest::from_tool_call("bash", &json!({"command": "echo hi"}), &cwd);
    assert!(engine.evaluate(&request).is_allow());
}

#[test]
fn cli_deny_wins_over_cli_allow() {
    let cwd = std::env::temp_dir();
    let flags = flags_from_args(&[
        "--permission-mode",
        "bypass",
        "--allow",
        "bash",
        "--deny",
        "bash",
    ]);
    let engine = PermissionEngine::from_settings_and_flags(&cwd, None, &flags).expect("engine");
    let request = PermissionRequest::from_tool_call("bash", &json!({"command": "echo hi"}), &cwd);
    assert!(engine.evaluate(&request).is_deny());
}

#[test]
fn cli_add_dir_extends_the_workspace() {
    let cwd = std::env::temp_dir();
    let extra = tempfile::tempdir().expect("extra");
    let flags = PermissionFlags {
        mode: Some(PermissionMode::AcceptEdits),
        add_dirs: vec![extra.path().to_path_buf()],
        ..Default::default()
    };
    let engine = PermissionEngine::from_settings_and_flags(&cwd, None, &flags).expect("engine");
    let request = PermissionRequest::from_tool_call(
        "write",
        &json!({"path": extra.path().join("out.txt")}),
        &cwd,
    );
    assert!(
        engine.evaluate(&request).is_allow(),
        "an explicit --add-dir must be writable in accept-edits"
    );
}

#[test]
fn headless_default_mode_still_returns_ask() {
    let cwd = std::env::temp_dir();
    let flags = flags_from_args(&["--permission-mode", "default"]);
    let engine = PermissionEngine::from_settings_and_flags(&cwd, None, &flags).expect("engine");
    let request = PermissionRequest::from_tool_call("bash", &json!({"command": "echo hi"}), &cwd);
    assert!(
        matches!(engine.evaluate(&request), PermissionDecision::Ask { .. }),
        "the CLI layer must not turn Ask into Allow; print/RPC has to fail closed"
    );
}

#[test]
fn explicit_allow_rule_is_visible_to_the_engine() {
    let cwd = std::env::temp_dir();
    let flags = flags_from_args(&[
        "--permission-mode",
        "read-only",
        "--allow",
        "command:git status",
    ]);
    let engine = PermissionEngine::from_settings_and_flags(&cwd, None, &flags).expect("engine");
    let request =
        PermissionRequest::from_tool_call("bash", &json!({"command": "git status --short"}), &cwd);
    assert!(engine.evaluate(&request).is_allow());
    // A different command is still read-only blocked.
    let other = PermissionRequest::from_tool_call("bash", &json!({"command": "git push"}), &cwd);
    assert!(engine.evaluate(&other).is_deny());
}

/// `--deny --permission-mode read-only` 不能把 `--permission-mode` 当成规则值：
/// 模式必须生效，且不能凭空产生一条 deny 规则。
#[test]
fn permission_flag_values_do_not_swallow_following_options() {
    let cwd = std::env::temp_dir();
    let flags = flags_from_args(&["--deny", "--permission-mode", "read-only"]);
    assert_eq!(flags.mode, Some(PermissionMode::ReadOnly));
    assert!(flags.deny.is_empty(), "{:?}", flags.deny);
    let engine = PermissionEngine::from_settings_and_flags(&cwd, None, &flags).expect("engine");
    let request = PermissionRequest::from_tool_call("bash", &json!({"command": "echo hi"}), &cwd);
    assert!(engine.evaluate(&request).is_deny());
}

#[test]
fn runtime_rule_decisions_still_respect_deny() {
    // 与 `AgentSession::add_permission_rules` 使用同一套规则语义：后加的 Allow 不会
    // 盖过先加的 Deny。
    let cwd = std::env::temp_dir();
    let engine = PermissionEngine::from_settings_and_flags(
        &cwd,
        None,
        &PermissionFlags {
            mode: Some(PermissionMode::BypassPermissions),
            ..Default::default()
        },
    )
    .expect("engine");
    engine.add_rules(vec![
        kiana_permissions::PermissionRule::tool("bash", RuleDecision::Allow),
        kiana_permissions::PermissionRule::tool("bash", RuleDecision::Deny),
    ]);
    let request = PermissionRequest::from_tool_call("bash", &json!({"command": "echo hi"}), &cwd);
    assert!(engine.evaluate(&request).is_deny());
}
