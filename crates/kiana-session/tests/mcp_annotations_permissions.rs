//! 第 5 周：MCP annotations 在**会话层**的验收测试。
//!
//! 纯策略函数（`kiana_resources::mcp_annotations`）已有单测；这里从 `AgentSession::prompt`
//! 走完整生产路径：模型发起 `mcp__*` 工具调用 → 权限引擎 → annotations 收紧/放行 →
//! `PermissionDenied`/工具结果。覆盖：
//!
//! - `destructiveHint` 强制 ask（无 UI 时 fail-closed）；
//! - `readOnlyHint` 只在 read-only 模式放行；
//! - 显式 deny 规则优先于任何 hint；
//! - destructive 工具的 `AllowAlways` 被拒绝（不产生持久授权）。

mod common;

use common::*;
use indexmap::IndexMap;
use kiana_ai::providers::faux::{FauxAssistantMessageOptions, FauxResponseStep, faux_tool_call};
use kiana_ai::types::StopReason;
use kiana_config::settings::schema::Settings;
use kiana_ext_api::tool_types::ToolAnnotations;
use kiana_session::agent_session::{AgentSessionEvent, ExtensionBindings, PromptOptions};
use serde_json::json;

fn settings_with_permissions(permissions: serde_json::Value) -> Settings {
    let mut settings = Settings::default();
    settings
        .extra
        .insert("permissions".to_string(), permissions);
    settings
}

/// 带 annotations 的 SDK 自定义工具（名字走 MCP 分类路径）。
fn annotated_tool(name: &str, annotations: ToolAnnotations) -> kiana_extensions::ToolDefinition {
    let mut definition = simple_tool_definition(name, "fixture MCP tool", None);
    definition.annotations = Some(annotations);
    definition
}

fn read_only_tool(name: &str) -> kiana_extensions::ToolDefinition {
    annotated_tool(
        name,
        ToolAnnotations {
            read_only_hint: Some(true),
            destructive_hint: None,
            idempotent_hint: None,
            open_world_hint: None,
        },
    )
}

fn destructive_tool(name: &str) -> kiana_extensions::ToolDefinition {
    annotated_tool(
        name,
        ToolAnnotations {
            read_only_hint: None,
            destructive_hint: Some(true),
            idempotent_hint: None,
            open_world_hint: None,
        },
    )
}

fn tool_call_step(name: &str) -> FauxResponseStep {
    FauxResponseStep::Message(kiana_ai::providers::faux::faux_assistant_message(
        faux_tool_call(name, IndexMap::new()),
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        },
    ))
}

fn tool_result_text(harness: &Harness, name: &str) -> String {
    get_message_text(&kiana_agent_core::types::AgentMessage::Standard(
        kiana_ai::types::Message::ToolResult(get_tool_result(harness, name)),
    ))
}

fn permission_denied_reasons(harness: &Harness) -> Vec<String> {
    harness
        .events_of_type("permission_denied")
        .into_iter()
        .filter_map(|event| match event {
            AgentSessionEvent::PermissionDenied { reason, .. } => Some(reason),
            _ => None,
        })
        .collect()
}

/// `readOnlyHint = true` 在 read-only 模式把模式级拒绝放宽为 allow，工具真正执行。
#[tokio::test]
async fn read_only_hint_allows_an_mcp_tool_in_read_only_mode() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({"mode": "read-only"}))),
            custom_tools: vec![read_only_tool("mcp__fixture__read")],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            tool_call_step("mcp__fixture__read"),
            faux_text_response("done"),
        ]);
        harness
            .session
            .prompt("read it".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        assert!(permission_denied_reasons(&harness).is_empty());
        assert_eq!(
            harness.events_of_type("permission_requested").len(),
            0,
            "readOnlyHint must allow without asking in read-only mode"
        );
        assert_eq!(tool_result_text(&harness, "mcp__fixture__read"), "ok");
        harness.cleanup();
    })
    .await;
}

/// `readOnlyHint` 在 default 模式**不**放行：仍然 ask；无 UI 时 fail-closed。
#[tokio::test]
async fn read_only_hint_does_not_allow_in_default_mode() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({"mode": "default"}))),
            custom_tools: vec![read_only_tool("mcp__fixture__read")],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            tool_call_step("mcp__fixture__read"),
            faux_text_response("done"),
        ]);
        harness
            .session
            .prompt("read it".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        assert_eq!(harness.events_of_type("permission_requested").len(), 1);
        assert_eq!(
            permission_denied_reasons(&harness).len(),
            1,
            "no UI means fail-closed"
        );
        assert!(
            tool_result_text(&harness, "mcp__fixture__read").contains("No interactive UI"),
            "tool must be blocked instead of executed"
        );
        harness.cleanup();
    })
    .await;
}

/// `destructiveHint = true` 即使有 Allow 规则也强制 ask（无 UI 时拒绝）。
#[tokio::test]
async fn destructive_hint_forces_ask_even_with_an_allow_rule() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({
                "mode": "default",
                "rules": [{
                    "tool": "mcp__fixture__destroy",
                    "decision": "allow",
                    "reason": "allow fixture"
                }]
            }))),
            custom_tools: vec![destructive_tool("mcp__fixture__destroy")],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            tool_call_step("mcp__fixture__destroy"),
            faux_text_response("done"),
        ]);
        harness
            .session
            .prompt("destroy it".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        assert_eq!(
            harness.events_of_type("permission_requested").len(),
            1,
            "destructiveHint must ask despite the allow rule"
        );
        let denied = permission_denied_reasons(&harness);
        assert_eq!(denied.len(), 1);
        assert!(denied[0].contains("destructiveHint"), "{}", denied[0]);
        let text = tool_result_text(&harness, "mcp__fixture__destroy");
        assert!(text.contains("destructiveHint"), "{text}");
        assert!(!text.contains("ok"), "tool must not execute: {text}");
        harness.cleanup();
    })
    .await;
}

/// 显式 deny 规则永远优先：hint 不能把 deny 翻成 allow。
#[tokio::test]
async fn explicit_deny_rule_wins_over_read_only_hint() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({
                "mode": "read-only",
                "rules": [{
                    "tool": "mcp__fixture__read",
                    "decision": "deny",
                    "reason": "managed deny"
                }]
            }))),
            custom_tools: vec![read_only_tool("mcp__fixture__read")],
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            tool_call_step("mcp__fixture__read"),
            faux_text_response("done"),
        ]);
        harness
            .session
            .prompt("read it".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        let denied = permission_denied_reasons(&harness);
        assert_eq!(denied.len(), 1);
        assert!(denied[0].contains("managed deny"), "{}", denied[0]);
        assert!(!tool_result_text(&harness, "mcp__fixture__read").contains("ok"));
        harness.cleanup();
    })
    .await;
}

/// destructive 工具的 `AllowAlways` 不能落成持久授权：本次拒绝，下一次仍然询问。
#[tokio::test]
async fn destructive_tool_cannot_persist_allow_always() {
    run(async {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_with_permissions(json!({"mode": "default"}))),
            custom_tools: vec![destructive_tool("mcp__fixture__destroy")],
            ..Default::default()
        })
        .await;
        // UI 无视 always_scopes 强行选择“Always allow”；会话/审批层必须拒绝持久化。
        let ui = ScriptedUi::new(
            vec![
                Some("Always allow".to_string()),
                Some("Always allow".to_string()),
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
        harness.set_responses(vec![
            tool_call_step("mcp__fixture__destroy"),
            faux_text_response("first"),
            tool_call_step("mcp__fixture__destroy"),
            faux_text_response("second"),
        ]);
        harness
            .session
            .prompt("destroy once".to_string(), PromptOptions::default())
            .await
            .expect("first prompt");
        assert!(
            tool_result_text(&harness, "mcp__fixture__destroy").contains("destructiveHint")
                || permission_denied_reasons(&harness)
                    .iter()
                    .any(|reason| reason.contains("persistent approval")),
            "the forced Always allow must not execute the destructive tool"
        );
        harness
            .session
            .prompt("destroy twice".to_string(), PromptOptions::default())
            .await
            .expect("second prompt");

        assert_eq!(
            harness.events_of_type("permission_requested").len(),
            2,
            "no persistent rule may be created for a destructive MCP tool"
        );
        let scopes: Vec<Option<String>> = harness
            .events_of_type("permission_decided")
            .into_iter()
            .filter_map(|event| match event {
                AgentSessionEvent::PermissionDecided { scope, .. } => Some(scope),
                _ => None,
            })
            .collect();
        assert!(
            scopes.iter().all(Option::is_none),
            "rejections must not record an approval scope: {scopes:?}"
        );
        harness.cleanup();
    })
    .await;
}
