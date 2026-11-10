//! 权限引擎与 `AgentSession` 的集成测试。
//!
//! 覆盖 PORTING_PLAN 第 0/1 周：settings 的 `permissions.mode` 能启用权限层；
//! `read-only` 直接拒绝、`default` 无 UI 时 fail-closed、`bypass` 放行；运行期
//! `set_permission_mode` 立即生效；`Deny` 规则永远优先于 `bypass`；结构化审批
//! （AllowOnce / AllowAlways{Session|Local|Project} / Reject）与事件流。

mod common;

use common::*;
use indexmap::IndexMap;
use kiana_agent_core::types::AgentMessage;
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message, faux_tool_call,
};
use kiana_ai::types::{Message, StopReason};
use kiana_config::settings::schema::Settings;
use kiana_permissions::{
    PermissionEngine, PermissionFlags, PermissionMode, PermissionRequest, PermissionRule,
    RuleDecision,
};
use kiana_session::agent_session::{
    AgentSessionEvent, ExecuteBashOptions, ExtensionBindings, PromptOptions,
};
use serde_json::json;

fn settings_with_permissions(permissions: serde_json::Value) -> Settings {
    let mut settings = Settings::default();
    settings
        .extra
        .insert("permissions".to_string(), permissions);
    settings
}

/// settings 里的 `permissions.mode=read-only` 会让 `execute_bash` 直接拒绝。
#[tokio::test]
async fn execute_bash_is_denied_in_read_only_mode_from_settings() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({"mode": "read-only"}))),
            ..Default::default()
        })
        .await;
        assert_eq!(
            harness.session.permission_mode(),
            Some(PermissionMode::ReadOnly)
        );

        let error = harness
            .session
            .execute_bash("echo should-not-run".to_string(), None, None)
            .await
            .expect_err("read-only must reject execute_bash");
        assert!(
            error.to_string().contains("read-only"),
            "unexpected error: {error}"
        );
        harness.cleanup();
    })
    .await;
}

/// managed policy 风格 `permissions.defaultMode=read-only` 必须启用 read-only 引擎，
/// 而不是因为只识别 `mode` 而把整层权限检查关掉。
#[tokio::test]
async fn default_mode_setting_activates_a_read_only_engine() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(
                json!({"defaultMode": "read-only"}),
            )),
            ..Default::default()
        })
        .await;
        assert_eq!(
            harness.session.permission_mode(),
            Some(PermissionMode::ReadOnly)
        );
        let error = harness
            .session
            .execute_bash("echo should-not-run".to_string(), None, None)
            .await
            .expect_err("defaultMode: read-only must reject execute_bash");
        assert!(
            error.to_string().contains("read-only"),
            "unexpected error: {error}"
        );
        harness.cleanup();
    })
    .await;
}

/// managed policy 的 `disableBypassPermissionsMode` 与 `mode: "off"` 同时出现时，
/// `off` 不能把整层权限检查关掉：引擎必须存在（default），managed deny 规则继续生效。
#[tokio::test]
async fn managed_disable_bypass_prevents_off_from_disabling_permissions() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({
                "mode": "off",
                "disableBypassPermissionsMode": true,
                "rules": [{"tool": "bash", "decision": "deny", "reason": "managed deny"}]
            }))),
            ..Default::default()
        })
        .await;
        assert_eq!(
            harness.session.permission_mode(),
            Some(PermissionMode::Default),
            "managed policy must keep an engine alive when user settings say off"
        );
        let error = harness
            .session
            .execute_bash("echo should-not-run".to_string(), None, None)
            .await
            .expect_err("managed deny must still reject execute_bash");
        assert!(
            error.to_string().contains("managed deny"),
            "unexpected error: {error}"
        );
        harness.cleanup();
    })
    .await;
}

/// `default` 模式在 print/headless（没有交互式 UI）下 fail-closed，而不是挂起。
#[tokio::test]
async fn execute_bash_default_mode_fails_closed_without_ui() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({"mode": "default"}))),
            ..Default::default()
        })
        .await;
        let error = harness
            .session
            .execute_bash("echo should-not-run".to_string(), None, None)
            .await
            .expect_err("default mode asks; without UI it must deny");
        assert!(
            error.to_string().contains("No interactive UI"),
            "unexpected error: {error}"
        );
        harness.cleanup();
    })
    .await;
}

/// `bypass` 模式放行，命令真的执行。
#[tokio::test]
async fn execute_bash_is_allowed_in_bypass_mode() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({"mode": "bypass"}))),
            ..Default::default()
        })
        .await;
        let result = harness
            .session
            .execute_bash(
                "echo permissions-ok".to_string(),
                None,
                Some(ExecuteBashOptions::default()),
            )
            .await
            .expect("bypass must allow execute_bash");
        assert!(
            result.output.contains("permissions-ok"),
            "unexpected output: {}",
            result.output
        );
        harness.cleanup();
    })
    .await;
}

/// 运行期切换模式立即生效。
#[tokio::test]
async fn set_permission_mode_takes_effect_immediately() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({"mode": "read-only"}))),
            ..Default::default()
        })
        .await;
        assert!(
            harness
                .session
                .execute_bash("echo nope".to_string(), None, None)
                .await
                .is_err()
        );

        harness
            .session
            .set_permission_mode(PermissionMode::BypassPermissions);
        let result = harness
            .session
            .execute_bash("echo switched".to_string(), None, None)
            .await
            .expect("bypass after runtime switch");
        assert!(result.output.contains("switched"));
        harness.cleanup();
    })
    .await;
}

/// 运行期追加的 `Deny` 规则优先于 `bypass`，也优先于 settings 里的 Allow。
#[tokio::test]
async fn deny_rule_added_at_runtime_still_wins() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({
                "mode": "bypass",
                "rules": [{"tool": "bash", "decision": "allow"}]
            }))),
            ..Default::default()
        })
        .await;
        harness
            .session
            .add_permission_rules([PermissionRule::tool("bash", RuleDecision::Deny)]);
        let error = harness
            .session
            .execute_bash("echo blocked".to_string(), None, None)
            .await
            .expect_err("deny must win over bypass");
        assert!(
            error.to_string().contains("Permission denied"),
            "unexpected error: {error}"
        );
        harness.cleanup();
    })
    .await;
}

/// `set_permission_rules` 从「没有引擎」的状态也能创建引擎。
#[tokio::test]
async fn set_permission_rules_creates_engine_when_absent() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        // 默认没有 settings/env 时引擎可能不存在；显式设置规则后必须创建。
        harness
            .session
            .set_permission_rules(vec![PermissionRule::tool("bash", RuleDecision::Deny)]);
        let error = harness
            .session
            .execute_bash("echo blocked".to_string(), None, None)
            .await
            .expect_err("deny rule must block execution");
        assert!(error.to_string().contains("Permission denied"));
        harness.cleanup();
    })
    .await;
}

// ---------------------------------------------------------------------------
// 第 1 周：结构化审批
// ---------------------------------------------------------------------------

/// `AllowOnce` 只放行本次，下一次同样的调用还会问。
#[tokio::test]
async fn allow_once_asks_again_on_the_next_identical_call() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({"mode": "default"}))),
            ..Default::default()
        })
        .await;
        let ui = ScriptedUi::new(
            vec![Some("Allow once".to_string()), Some("Reject".to_string())],
            vec![Some("not this time".to_string())],
        );
        harness
            .session
            .bind_extensions(ExtensionBindings {
                ui_context: Some(ui.clone()),
                ..Default::default()
            })
            .await;

        let first = harness
            .session
            .execute_bash("echo allow-once".to_string(), None, None)
            .await
            .expect("allow once must run the command");
        assert!(first.output.contains("allow-once"));

        let second = harness
            .session
            .execute_bash("echo allow-once".to_string(), None, None)
            .await
            .expect_err("a second identical call must ask again");
        assert!(
            second.to_string().contains("not this time"),
            "reject reason must reach the caller/model: {second}"
        );

        let requested = harness.events_of_type("permission_requested");
        assert_eq!(requested.len(), 2, "two asks = two requested events");
        let decided = harness.events_of_type("permission_decided");
        assert_eq!(decided.len(), 2);
        assert_eq!(harness.events_of_type("permission_denied").len(), 1);

        // 审批 UI 必须展示实际命令，而不是只显示 “default mode asks”。
        let titles = ui.select_titles.lock().unwrap().clone();
        assert_eq!(titles.len(), 2);
        assert!(
            titles[0].contains("bash") && titles[0].contains("echo allow-once"),
            "approval title must show the tool and command: {}",
            titles[0]
        );
        assert!(
            titles[1].contains("echo allow-once"),
            "second approval must also show the command: {}",
            titles[1]
        );
        harness.cleanup();
    })
    .await;
}

/// 显式 `permissions.ask` 规则命中时，「总是允许（本地）」必须真正生效：
/// 第二次相同的调用不再询问，且重启后规则仍然覆盖 Ask 规则。
#[tokio::test]
async fn always_allow_local_overrides_a_configured_ask_rule() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({
                "mode": "default",
                "ask": ["bash"]
            }))),
            ..Default::default()
        })
        .await;
        let ui = ScriptedUi::new(
            vec![
                Some("Always allow".to_string()),
                Some("This machine (local)".to_string()),
            ],
            Vec::new(),
        );
        harness
            .session
            .bind_extensions(ExtensionBindings {
                ui_context: Some(ui.clone()),
                ..Default::default()
            })
            .await;

        harness
            .session
            .execute_bash("echo ask-rule".to_string(), None, None)
            .await
            .expect("first call is approved");
        // 第一次的 Ask 必须来自配置里的 Ask 规则（而不是模式默认），否则这个测试
        // 不能证明持久化批准覆盖了 Ask 规则。
        let first_title = ui.select_titles.lock().unwrap()[0].clone();
        assert!(
            first_title.contains("Ask rule matched"),
            "first prompt must come from the configured ask rule: {first_title}"
        );
        harness
            .session
            .execute_bash("echo ask-rule".to_string(), None, None)
            .await
            .expect("persisted local approval must override the configured ask rule");
        assert_eq!(
            ui.select_count(),
            2,
            "the second call must not ask again (only choice + scope on the first call)"
        );

        // 模拟重启：只用文件里的 permissions + 原始 Ask 规则构造引擎。
        let path =
            kiana_config::settings::permissions::local_settings_path(harness.temp_dir.path());
        let permissions = kiana_config::settings::permissions::read_permissions_file(&path)
            .expect("read local settings")
            .expect("permissions written");
        let merged = kiana_config::settings::permissions::merge_permission_settings(
            Some(&json!({"mode": "default", "ask": ["bash"]})),
            Some(&permissions),
        );
        let engine = PermissionEngine::from_settings_and_flags(
            harness.temp_dir.path(),
            merged.as_ref(),
            &PermissionFlags::default(),
        )
        .expect("persisted approval enables the engine");
        let request = PermissionRequest::from_tool_call(
            "bash",
            &json!({"command": "echo ask-rule"}),
            harness.temp_dir.path(),
        );
        assert!(
            engine.evaluate(&request).is_allow(),
            "after restart the approved rule must still override the ask rule"
        );
        harness.cleanup();
    })
    .await;
}

/// `.git`/`.kiana` 安全确认：UI 不提供「总是允许」，且每次都必须重新询问。
#[tokio::test]
async fn protected_git_writes_always_prompt_and_never_offer_always_allow() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({"mode": "default"}))),
            ..Default::default()
        })
        .await;
        std::fs::create_dir_all(harness.temp_dir.path().join(".git/hooks"))
            .expect("create .git/hooks");
        let ui = ScriptedUi::new(
            vec![
                Some("Allow once".to_string()),
                Some("Allow once".to_string()),
            ],
            Vec::new(),
        );
        harness
            .session
            .bind_extensions(ExtensionBindings {
                ui_context: Some(ui.clone()),
                ..Default::default()
            })
            .await;

        harness
            .session
            .execute_bash("echo x > .git/hooks/pre-commit".to_string(), None, None)
            .await
            .expect("allow once runs the protected write");
        harness
            .session
            .execute_bash("echo x > .git/hooks/pre-commit".to_string(), None, None)
            .await
            .expect("second allow once runs the protected write");

        assert_eq!(ui.select_count(), 2, "every protected write must ask again");
        let options = ui.select_options.lock().unwrap().clone();
        assert!(
            options
                .iter()
                .all(|options| !options.iter().any(|option| option == "Always allow")),
            "safety-check asks must not offer Always allow: {options:?}"
        );
        harness.cleanup();
    })
    .await;
}

/// 项目 settings 文件损坏时，「总是允许（项目）」必须 fail-closed：
/// 返回错误、发 permission_denied、不覆盖损坏文件，重启后也没有这条规则。
#[tokio::test]
async fn project_always_allow_fails_closed_when_project_settings_are_corrupt() {
    run(async {
        use std::sync::Arc;

        let project = tempfile::tempdir().expect("project dir");
        let agent = tempfile::tempdir().expect("agent dir");
        std::fs::create_dir_all(project.path().join(".kiana")).expect("project config dir");
        let project_path = project.path().join(".kiana/settings.json");
        std::fs::write(&project_path, "{ not valid json").expect("corrupt project settings");
        std::fs::write(
            agent.path().join("settings.json"),
            serde_json::to_string_pretty(&json!({
                "permissions": {"mode": "default"}
            }))
            .expect("serialize global settings"),
        )
        .expect("global settings");

        let storage: Arc<dyn kiana_config::settings::SettingsStorage> =
            Arc::new(kiana_config::settings::FileSettingsStorage::new(
                &project.path().to_string_lossy(),
                &agent.path().to_string_lossy(),
            ));
        let manager = kiana_config::settings::SettingsManager::from_storage(Arc::clone(&storage));
        let harness = create_harness(HarnessOptions {
            settings_manager: Some(manager),
            ..Default::default()
        })
        .await;

        let ui = ScriptedUi::new(
            vec![
                Some("Always allow".to_string()),
                Some("This project".to_string()),
            ],
            Vec::new(),
        );
        harness
            .session
            .bind_extensions(ExtensionBindings {
                ui_context: Some(ui),
                ..Default::default()
            })
            .await;

        let error = harness
            .session
            .execute_bash("echo project-corrupt".to_string(), None, None)
            .await
            .expect_err("project always-allow must fail when the file cannot be loaded");
        assert!(
            error.to_string().contains("failed to load"),
            "unexpected error: {error}"
        );
        assert_eq!(
            harness.events_of_type("permission_denied").len(),
            1,
            "the failed persistence must emit permission_denied"
        );
        assert_eq!(
            std::fs::read_to_string(&project_path).expect("read project settings"),
            "{ not valid json",
            "a file that failed to load must not be overwritten"
        );

        // 重启后规则不存在：新的 manager 仍然看不到 permissions，引擎重新询问。
        let restarted = kiana_config::settings::SettingsManager::from_storage(storage);
        assert!(restarted.project_permissions().is_none());
        let engine = PermissionEngine::from_settings_and_flags(
            project.path(),
            Some(&json!({"mode": "default"})),
            &PermissionFlags::default(),
        )
        .expect("engine");
        let request = PermissionRequest::from_tool_call(
            "bash",
            &json!({"command": "echo project-corrupt"}),
            harness.temp_dir.path(),
        );
        assert!(engine.evaluate(&request).is_ask());
        harness.cleanup();
    })
    .await;
}

/// `AllowAlways{Session}` 之后，同一命令不再询问。
#[tokio::test]
async fn allow_always_session_stops_further_prompts() {
    run(async {
        let harness = create_harness(HarnessOptions {
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
        harness
            .session
            .bind_extensions(ExtensionBindings {
                ui_context: Some(ui.clone()),
                ..Default::default()
            })
            .await;

        harness
            .session
            .execute_bash("echo session-once".to_string(), None, None)
            .await
            .expect("first call approved");
        harness
            .session
            .execute_bash("echo session-once".to_string(), None, None)
            .await
            .expect("second call must not ask again");

        assert_eq!(
            ui.select_count(),
            2,
            "only the first call should open the two selectors (choice + scope)"
        );
        harness.cleanup();
    })
    .await;
}

/// `AllowAlways{Local}` 把规则写进项目本地 settings，重启后仍可匹配。
#[tokio::test]
async fn allow_always_local_persists_a_rule_to_local_settings() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({"mode": "default"}))),
            ..Default::default()
        })
        .await;
        let ui = ScriptedUi::new(
            vec![
                Some("Always allow".to_string()),
                Some("This machine (local)".to_string()),
            ],
            Vec::new(),
        );
        harness
            .session
            .bind_extensions(ExtensionBindings {
                ui_context: Some(ui.clone()),
                ..Default::default()
            })
            .await;
        harness
            .session
            .execute_bash("echo local-once".to_string(), None, None)
            .await
            .expect("local always-allow must run the command");

        let path =
            kiana_config::settings::permissions::local_settings_path(harness.temp_dir.path());
        let permissions = kiana_config::settings::permissions::read_permissions_file(&path)
            .expect("read local settings")
            .expect("permissions written");
        let rules = permissions["rules"].as_array().expect("rules");
        assert_eq!(rules.len(), 1, "{permissions}");
        // 新规则建议只生成「已知 program + subcommand」前缀；`echo` 没有已知子命令，
        // 因此保存为精确命令（字符串 + `exact: true`）。
        assert_eq!(rules[0]["command"], json!("echo local-once"));
        assert_eq!(rules[0]["decision"], json!("allow"));

        // 模拟重启：只用文件里的 permissions 构造引擎，同一命令应直接 Allow。
        let merged = kiana_config::settings::permissions::merge_permission_settings(
            Some(&json!({"mode": "default"})),
            Some(&permissions),
        );
        let engine = PermissionEngine::from_settings_and_flags(
            harness.temp_dir.path(),
            merged.as_ref(),
            &PermissionFlags::default(),
        )
        .expect("persisted rules enable the engine");
        let request = PermissionRequest::from_tool_call(
            "bash",
            &json!({"command": "echo local-once"}),
            harness.temp_dir.path(),
        );
        assert!(engine.evaluate(&request).is_allow());

        assert_eq!(
            harness.events_of_type("permission_decided")[0].clone(),
            AgentSessionEvent::PermissionDecided {
                tool_name: "bash".to_string(),
                decision: "allow-always".to_string(),
                scope: Some("local".to_string()),
                reason: None,
                target: "echo local-once".to_string(),
            }
        );
        harness.cleanup();
    })
    .await;
}

/// `AllowAlways{Project}` 写项目 settings（这里用 in-memory 存储验证调用链）。
#[tokio::test]
async fn allow_always_project_updates_project_settings() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({"mode": "default"}))),
            ..Default::default()
        })
        .await;
        let ui = ScriptedUi::new(
            vec![
                Some("Always allow".to_string()),
                Some("This project".to_string()),
            ],
            Vec::new(),
        );
        harness
            .session
            .bind_extensions(ExtensionBindings {
                ui_context: Some(ui.clone()),
                ..Default::default()
            })
            .await;
        harness
            .session
            .execute_bash("echo project-once".to_string(), None, None)
            .await
            .expect("project always-allow must run the command");

        harness.settings_manager.flush().await;
        let permissions = harness
            .settings_manager
            .project_permissions()
            .expect("project permissions written");
        let rules = permissions["rules"].as_array().expect("rules");
        assert!(
            rules
                .iter()
                .any(|rule| rule["command"] == json!("echo project-once")),
            "{permissions}"
        );
        harness.cleanup();
    })
    .await;
}

/// 审计事件里的参数摘要会脱敏 secret。
#[tokio::test]
async fn permission_events_redact_secrets() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({"mode": "default"}))),
            ..Default::default()
        })
        .await;
        let ui = ScriptedUi::new(vec![Some("Reject".to_string())], Vec::new());
        harness
            .session
            .bind_extensions(ExtensionBindings {
                ui_context: Some(ui),
                ..Default::default()
            })
            .await;
        let _ = harness
            .session
            .execute_bash("echo token=sk-super-secret".to_string(), None, None)
            .await;
        let requested = harness.events_of_type("permission_requested");
        assert_eq!(requested.len(), 1);
        let AgentSessionEvent::PermissionRequested {
            summary, reason, ..
        } = &requested[0]
        else {
            panic!("unexpected event: {:?}", requested[0]);
        };
        assert!(!summary.contains("sk-super-secret"), "{summary}");
        assert!(summary.contains("[redacted]"), "{summary}");
        assert!(!reason.contains("sk-super-secret"), "{reason}");
        harness.cleanup();
    })
    .await;
}

/// 模型发起的 `bash` 工具调用与 `execute_bash` 使用同一套审批。
#[tokio::test]
async fn bash_tool_call_uses_the_same_approval_flow() {
    run(async {
        let harness = create_harness(HarnessOptions {
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
        harness
            .session
            .bind_extensions(ExtensionBindings {
                ui_context: Some(ui),
                ..Default::default()
            })
            .await;
        let mut args = IndexMap::new();
        args.insert("command".to_string(), json!("echo tool-approved"));
        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                faux_tool_call("bash", args),
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::ToolUse),
                    ..Default::default()
                },
            )),
            faux_text_response("done"),
        ]);
        harness
            .session
            .prompt("run it".to_string(), PromptOptions::default())
            .await
            .expect("prompt");
        let result = get_tool_result(&harness, "bash");
        let text = get_message_text(&AgentMessage::Standard(Message::ToolResult(result)));
        assert!(text.contains("tool-approved"), "{text}");
        assert_eq!(
            harness.events_of_type("permission_requested").len(),
            1,
            "the tool call must go through exactly one approval"
        );
        harness.cleanup();
    })
    .await;
}
