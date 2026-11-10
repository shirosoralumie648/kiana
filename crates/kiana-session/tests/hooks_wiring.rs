//! 第 4/7 周共享接线：hooks 生命周期、顺序契约与 apply_patch 模型路由的会话级测试。
//!
//! 这些用例走完整的 `AgentSession`（`create_harness`），验证共享文件里的接线：
//! PreToolUse 改写先于权限判断、hook deny 只出现一次、PostToolUse feedback 合并、
//! Stop deny 续回合、审计记录，以及 `tools.applyPatch.models` 的工具路由。

mod common;

use common::{
    Harness, HarnessOptions, create_harness, faux_text_response, run, settings_from_json,
};
use kiana_agent_core::types::AgentMessage;
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message, faux_tool_call,
};
use kiana_ai::types::{Message, StopReason, ToolResultMessage};
use kiana_hooks::HookEvent;
use kiana_session::agent_session::{AgentSession, PromptOptions};
use serde_json::{Value, json};

fn tool_result_text(result: &ToolResultMessage) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            kiana_ai::types::UserContentBlock::Text(text) => Some(text.text.clone()),
            kiana_ai::types::UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn message_text(message: &AgentMessage) -> String {
    match message {
        AgentMessage::Standard(Message::User(user)) => match &user.content {
            kiana_ai::types::UserContent::Text(text) => text.clone(),
            kiana_ai::types::UserContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    kiana_ai::types::UserContentBlock::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
        },
        AgentMessage::Standard(Message::Assistant(assistant)) => assistant
            .content
            .iter()
            .filter_map(|block| match block {
                kiana_ai::types::AssistantContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        AgentMessage::Standard(Message::ToolResult(result)) => tool_result_text(result),
        AgentMessage::Standard(Message::System(system)) => format!("{system:?}"),
        AgentMessage::Custom(_) => String::new(),
    }
}

fn tool_results(session: &AgentSession) -> Vec<ToolResultMessage> {
    session
        .messages()
        .into_iter()
        .filter_map(|message| match message {
            AgentMessage::Standard(Message::ToolResult(result)) => Some(result),
            _ => None,
        })
        .collect()
}

fn message_texts(session: &AgentSession) -> Vec<String> {
    session.messages().iter().map(message_text).collect()
}

fn tool_call_step(command: &str) -> FauxResponseStep {
    let arguments = [("command".to_string(), json!(command))]
        .into_iter()
        .collect::<indexmap::IndexMap<_, _>>();
    FauxResponseStep::Message(faux_assistant_message(
        faux_tool_call("bash", arguments),
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        },
    ))
}

fn text_step(text: &str) -> FauxResponseStep {
    faux_assistant_message(text, Default::default()).into()
}

async fn create_session(settings_json: Value, files: &[(&str, &str)]) -> Harness {
    let harness = create_harness(HarnessOptions {
        settings: Some(settings_from_json(settings_json)),
        ..Default::default()
    })
    .await;
    for (name, content) in files {
        std::fs::write(harness.temp_dir.path().join(name), content).expect("write hook script");
    }
    harness
}

fn settings_with_hooks(hooks: Value, permissions: Option<Value>, tools: Option<Value>) -> Value {
    let mut root = serde_json::Map::new();
    root.insert("hooks".to_string(), hooks);
    if let Some(permissions) = permissions {
        root.insert("permissions".to_string(), permissions);
    }
    if let Some(tools) = tools {
        root.insert("tools".to_string(), tools);
    }
    Value::Object(root)
}

/// PreToolUse 的 `updatedInput` 必须在权限判断之前整体替换参数：只有改写后的命令
/// `echo denied` 命中 deny 规则，原命令 `echo original` 不在规则内。
#[tokio::test]
async fn pre_tool_use_updated_input_is_seen_by_permission_layer() {
    run(async {
        let harness = create_session(
            settings_with_hooks(
                json!({
                    "PreToolUse": [{
                        "matcher": "bash",
                        "hooks": [{
                            "type": "command",
                            "command": "sh .hook-rewrite.sh"
                        }]
                    }]
                }),
                Some(json!({
                    "mode": "bypass",
                    "rules": [{
                        "command": ["echo", "denied"],
                        "decision": "deny",
                        "reason": "denied by rule"
                    }]
                })),
                None,
            ),
            &[(
                ".hook-rewrite.sh",
                "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"updatedInput\":{\"command\":\"echo denied\"}}}'\n",
            )],
        )
        .await;
        harness.set_responses(vec![tool_call_step("echo original"), text_step("done")]);

        harness
            .session
            .prompt("run it".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        let results = tool_results(&harness.session);
        assert_eq!(results.len(), 1, "one tool result expected");
        let text = tool_result_text(&results[0]);
        assert!(results[0].is_error, "permission deny must fail the tool");
        assert!(
            text.contains("denied by rule"),
            "permission layer must see the hook-rewritten command: {text}"
        );

        let audit = harness.session.hook_audit_records();
        assert!(
            audit
                .iter()
                .any(|record| record.event == HookEvent::PreToolUse),
            "PreToolUse hook should be audited: {audit:?}"
        );
        harness.cleanup();
    })
    .await;
}

/// PreToolUse exit 2：工具不执行，stderr 作为模型可见错误出现一次，审计记为 blocked。
#[tokio::test]
async fn pre_tool_use_exit_two_blocks_before_execution() {
    run(async {
        let harness = create_session(
            settings_with_hooks(
                json!({
                    "PreToolUse": [{
                        "matcher": "bash",
                        "hooks": [{
                            "type": "command",
                            "command": "sh .hook-deny.sh"
                        }]
                    }]
                }),
                Some(json!({"mode": "bypass"})),
                None,
            ),
            &[(
                ".hook-deny.sh",
                "#!/bin/sh\ncat >/dev/null\necho 'blocked by hook' >&2\nexit 2\n",
            )],
        )
        .await;
        harness.set_responses(vec![
            tool_call_step("touch .should-not-exist"),
            text_step("done"),
        ]);

        harness
            .session
            .prompt("run it".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        let results = tool_results(&harness.session);
        assert_eq!(results.len(), 1);
        let text = tool_result_text(&results[0]);
        assert!(results[0].is_error, "hook deny must fail the tool");
        assert_eq!(
            text.matches("blocked by hook").count(),
            1,
            "blocking error must appear exactly once: {text}"
        );
        assert!(
            !harness.temp_dir.path().join(".should-not-exist").exists(),
            "denied tool must not execute"
        );

        let audit = harness.session.hook_audit_records();
        assert!(
            audit.iter().any(|record| {
                record.event == HookEvent::PreToolUse
                    && record.outcome == kiana_hooks::HookAuditOutcome::Blocked
            }),
            "deny should be audited as blocked: {audit:?}"
        );
        harness.cleanup();
    })
    .await;
}

/// PostToolUse exit 2 的 stderr 作为模型反馈追加到工具结果，但不把成功结果改成失败。
#[tokio::test]
async fn post_tool_use_feedback_is_appended_to_the_result() {
    run(async {
        let harness = create_session(
            settings_with_hooks(
                json!({
                    "PostToolUse": [{
                        "matcher": "bash",
                        "hooks": [{
                            "type": "command",
                            "command": "sh .hook-post.sh"
                        }]
                    }]
                }),
                Some(json!({"mode": "bypass"})),
                None,
            ),
            &[(
                ".hook-post.sh",
                "#!/bin/sh\ncat >/dev/null\necho 'post feedback' >&2\nexit 2\n",
            )],
        )
        .await;
        harness.set_responses(vec![tool_call_step("echo hello"), text_step("done")]);

        harness
            .session
            .prompt("run it".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        let results = tool_results(&harness.session);
        assert_eq!(results.len(), 1);
        let text = tool_result_text(&results[0]);
        assert!(text.contains("hello"), "original output kept: {text}");
        assert!(
            text.contains("post feedback"),
            "PostToolUse feedback must be appended: {text}"
        );
        assert!(
            !results[0].is_error,
            "PostToolUse exit 2 is feedback, not a tool failure"
        );
        harness.cleanup();
    })
    .await;
}

/// Stop exit 2：hook deny 表示不要结束回合，原因作为 steering 消息进入下一轮请求。
#[tokio::test]
async fn stop_hook_deny_continues_the_turn_once() {
    run(async {
        let harness = create_session(
            settings_with_hooks(
                json!({
                    "Stop": [{
                        "hooks": [{
                            "type": "command",
                            "command": "sh .hook-stop.sh"
                        }]
                    }]
                }),
                Some(json!({"mode": "bypass"})),
                None,
            ),
            &[(
                ".hook-stop.sh",
                "#!/bin/sh\ncat >/dev/null\nif [ -f .stop-marker ]; then exit 0; fi\n: > .stop-marker\necho 'keep working' >&2\nexit 2\n",
            )],
        )
        .await;
        harness.set_responses(vec![text_step("first"), text_step("second")]);

        harness
            .session
            .prompt("go".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        let texts = message_texts(&harness.session);
        assert!(
            texts.iter().any(|text| text.contains("keep working")),
            "hook deny reason must be injected as a steering message: {texts:?}"
        );
        assert!(
            texts.iter().any(|text| text.contains("second")),
            "the run must continue to the next assistant turn: {texts:?}"
        );
        harness.cleanup();
    })
    .await;
}

/// `tools.applyPatch.models` 命中时编辑槽暴露 apply_patch，否则保留 edit；两者不同时出现。
#[tokio::test]
async fn apply_patch_model_routing_replaces_edit() {
    run(async {
        let routed = create_session(
            settings_with_hooks(
                json!({}),
                Some(json!({"mode": "bypass"})),
                Some(json!({"applyPatch": {"models": ["*"]}})),
            ),
            &[],
        )
        .await;
        let active = routed.session.get_active_tool_names();
        assert!(
            active.iter().any(|name| name == "apply_patch"),
            "apply_patch should be active for a matching model: {active:?}"
        );
        assert!(
            !active.iter().any(|name| name == "edit"),
            "edit must not be exposed at the same time: {active:?}"
        );
        routed.cleanup();

        let default = create_session(
            settings_with_hooks(json!({}), Some(json!({"mode": "bypass"})), None),
            &[],
        )
        .await;
        let active = default.session.get_active_tool_names();
        assert!(
            active.iter().any(|name| name == "edit"),
            "edit should stay active without a route: {active:?}"
        );
        assert!(
            !active.iter().any(|name| name == "apply_patch"),
            "apply_patch must not be active by default: {active:?}"
        );
        default.cleanup();
    })
    .await;
}

// ---------------------------------------------------------------------------
// 深度修复：hook 输出契约（用户可见警告、continue/stopReason、allow、additionalContext）
// ---------------------------------------------------------------------------

/// 从 provider 请求的消息里提取纯文本（用于断言 hidden context 注入）。
fn request_message_texts(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .map(|message| match message {
            Message::System(system) => match &system.content {
                kiana_ai::types::SystemContent::Text(text) => text.clone(),
                kiana_ai::types::SystemContent::Blocks(blocks) => blocks
                    .iter()
                    .map(|block| block.text.clone())
                    .collect::<Vec<_>>()
                    .join(""),
            },
            Message::User(user) => match &user.content {
                kiana_ai::types::UserContent::Text(text) => text.clone(),
                kiana_ai::types::UserContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|block| match block {
                        kiana_ai::types::UserContentBlock::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(""),
            },
            Message::Assistant(assistant) => assistant
                .content
                .iter()
                .filter_map(|block| match block {
                    kiana_ai::types::AssistantContentBlock::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
            Message::ToolResult(result) => result
                .content
                .iter()
                .filter_map(|block| match block {
                    kiana_ai::types::UserContentBlock::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
        })
        .collect()
}

/// 捕获下一次 provider 请求的工厂响应。
fn capturing_factory(
    sink: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    reply: &str,
) -> FauxResponseStep {
    let reply = reply.to_string();
    FauxResponseStep::message_factory(move |input| {
        let text = request_message_texts(&input.context.as_context().messages).join("\n");
        sink.lock().unwrap().push(text);
        std::future::ready(faux_assistant_message(reply.clone(), Default::default()))
    })
}

fn hook_notification_messages(harness: &Harness, level: &str) -> Vec<String> {
    harness
        .events_of_type("hook_notification")
        .into_iter()
        .filter_map(|event| match event {
            kiana_session::agent_session::AgentSessionEvent::HookNotification {
                message,
                level: event_level,
                ..
            } if event_level == level => Some(message),
            _ => None,
        })
        .collect()
}

/// exit 1：工具继续执行，stderr 警告通过 `hook_notification` 呈现给用户，审计保留
/// 警告文本。
#[tokio::test]
async fn exit_one_hook_warns_the_user_and_keeps_the_tool_running() {
    run(async {
        let harness = create_session(
            settings_with_hooks(
                json!({
                    "PreToolUse": [{
                        "matcher": "bash",
                        "hooks": [{
                            "type": "command",
                            "command": "sh .hook-warn.sh"
                        }]
                    }]
                }),
                Some(json!({"mode": "bypass"})),
                None,
            ),
            &[(
                ".hook-warn.sh",
                "#!/bin/sh\ncat >/dev/null\necho 'hook stderr warning' >&2\nexit 1\n",
            )],
        )
        .await;
        harness.set_responses(vec![tool_call_step("echo hello"), text_step("done")]);

        harness
            .session
            .prompt("run it".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        let results = tool_results(&harness.session);
        assert_eq!(results.len(), 1);
        assert!(!results[0].is_error, "exit 1 must not block the tool");
        assert!(tool_result_text(&results[0]).contains("hello"));

        let warnings = hook_notification_messages(&harness, "warning");
        assert!(
            warnings.iter().any(|message| {
                message.contains("hook stderr warning") && message.contains("status 1")
            }),
            "user must see the exit-1 stderr warning: {warnings:?}"
        );

        let audit = harness.session.hook_audit_records();
        let record = audit
            .iter()
            .find(|record| record.event == HookEvent::PreToolUse)
            .expect("PreToolUse audit record");
        assert!(
            record
                .warnings
                .iter()
                .any(|warning| warning.contains("hook stderr warning")),
            "audit record must retain warning text: {record:?}"
        );
        harness.cleanup();
    })
    .await;
}

/// PreToolUse `continue: false`：本次调用被阻断（terminate），run 不再继续。
#[tokio::test]
async fn pre_tool_use_continue_false_blocks_and_terminates_the_run() {
    run(async {
        let harness = create_session(
            settings_with_hooks(
                json!({
                    "PreToolUse": [{
                        "matcher": "bash",
                        "hooks": [{
                            "type": "command",
                            "command": "sh .hook-continue.sh"
                        }]
                    }]
                }),
                Some(json!({"mode": "bypass"})),
                None,
            ),
            &[(
                ".hook-continue.sh",
                "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"continue\":false,\"stopReason\":\"stop requested by hook\"}'\n",
            )],
        )
        .await;
        harness.set_responses(vec![
            tool_call_step("touch .should-not-exist"),
            text_step("must not run"),
        ]);

        harness
            .session
            .prompt("run it".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        let results = tool_results(&harness.session);
        assert_eq!(results.len(), 1);
        assert!(results[0].is_error, "continue=false must fail the call");
        assert!(
            tool_result_text(&results[0]).contains("stop requested by hook"),
            "stopReason must be model-visible: {}",
            tool_result_text(&results[0])
        );
        assert!(!harness.temp_dir.path().join(".should-not-exist").exists());
        assert!(
            !message_texts(&harness.session)
                .iter()
                .any(|text| text.contains("must not run")),
            "run must terminate after continue=false"
        );
        harness.cleanup();
    })
    .await;
}

/// PreToolUse `additionalContext` 必须进入下一次 provider 请求。
#[tokio::test]
async fn pre_tool_use_additional_context_reaches_the_next_request() {
    run(async {
        use std::sync::{Arc, Mutex};
        let harness = create_session(
            settings_with_hooks(
                json!({
                    "PreToolUse": [{
                        "matcher": "bash",
                        "hooks": [{
                            "type": "command",
                            "command": "sh .hook-context.sh"
                        }]
                    }]
                }),
                Some(json!({"mode": "bypass"})),
                None,
            ),
            &[(
                ".hook-context.sh",
                "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"additionalContext\":\"context from pre hook\"}}'\n",
            )],
        )
        .await;
        let captured = Arc::new(Mutex::new(Vec::new()));
        harness.set_responses(vec![
            tool_call_step("echo hi"),
            capturing_factory(Arc::clone(&captured), "done"),
        ]);

        harness
            .session
            .prompt("run it".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1, "one follow-up request expected");
        assert!(
            requests[0].contains("context from pre hook"),
            "PreToolUse additionalContext must be injected: {}",
            requests[0]
        );
        harness.cleanup();
    })
    .await;
}

/// hook 的 `permissionDecision: allow` 跳过权限层 Ask，但 deny 规则仍然优先。
#[tokio::test]
async fn hook_allow_skips_ask_but_not_deny() {
    run(async {
        let script = "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"allow\"}}'\n";
        // 1) Ask 规则 + hook allow：没有交互 UI 也能执行。
        let harness = create_session(
            settings_with_hooks(
                json!({
                    "PreToolUse": [{
                        "matcher": "bash",
                        "hooks": [{"type": "command", "command": "sh .hook-allow.sh"}]
                    }]
                }),
                Some(json!({"mode": "default", "ask": ["bash"]})),
                None,
            ),
            &[(".hook-allow.sh", script)],
        )
        .await;
        harness.set_responses(vec![tool_call_step("echo allowed"), text_step("done")]);
        harness
            .session
            .prompt("run it".to_string(), PromptOptions::default())
            .await
            .expect("prompt");
        let results = tool_results(&harness.session);
        assert_eq!(results.len(), 1);
        assert!(
            !results[0].is_error,
            "hook allow must skip the Ask gate: {}",
            tool_result_text(&results[0])
        );
        assert!(tool_result_text(&results[0]).contains("allowed"));
        harness.cleanup();

        // 2) deny 规则 + hook allow：仍然拒绝。
        let harness = create_session(
            settings_with_hooks(
                json!({
                    "PreToolUse": [{
                        "matcher": "bash",
                        "hooks": [{"type": "command", "command": "sh .hook-allow.sh"}]
                    }]
                }),
                Some(json!({
                    "mode": "bypass",
                    "rules": [{
                        "command": ["echo", "denied"],
                        "decision": "deny",
                        "reason": "denied by rule"
                    }]
                })),
                None,
            ),
            &[(".hook-allow.sh", script)],
        )
        .await;
        harness.set_responses(vec![tool_call_step("echo denied"), text_step("done")]);
        harness
            .session
            .prompt("run it".to_string(), PromptOptions::default())
            .await
            .expect("prompt");
        let results = tool_results(&harness.session);
        assert_eq!(results.len(), 1);
        assert!(results[0].is_error, "hook allow must not override deny");
        assert!(tool_result_text(&results[0]).contains("denied by rule"));
        harness.cleanup();
    })
    .await;
}

/// PostToolUse `continue: false`：工具结果保留，但 run 在批次结束后停止。
#[tokio::test]
async fn post_tool_use_continue_false_terminates_the_run() {
    run(async {
        let harness = create_session(
            settings_with_hooks(
                json!({
                    "PostToolUse": [{
                        "matcher": "bash",
                        "hooks": [{
                            "type": "command",
                            "command": "sh .hook-post-stop.sh"
                        }]
                    }]
                }),
                Some(json!({"mode": "bypass"})),
                None,
            ),
            &[(
                ".hook-post-stop.sh",
                "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"continue\":false,\"stopReason\":\"post stop requested\"}'\n",
            )],
        )
        .await;
        harness.set_responses(vec![
            tool_call_step("echo hello"),
            text_step("must not run"),
        ]);

        harness
            .session
            .prompt("run it".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        let results = tool_results(&harness.session);
        assert_eq!(results.len(), 1);
        assert!(tool_result_text(&results[0]).contains("hello"));
        assert!(
            tool_result_text(&results[0]).contains("post stop requested"),
            "stopReason must be appended to the tool result"
        );
        assert!(
            !message_texts(&harness.session)
                .iter()
                .any(|text| text.contains("must not run")),
            "PostToolUse continue=false must stop the run"
        );
        harness.cleanup();
    })
    .await;
}

/// Stop hook 的 `continue: false` 阻止下一轮。
#[tokio::test]
async fn stop_hook_continue_false_stops_the_run() {
    run(async {
        let harness = create_session(
            settings_with_hooks(
                json!({
                    "Stop": [{
                        "hooks": [{
                            "type": "command",
                            "command": "sh .hook-stop-continue.sh"
                        }]
                    }]
                }),
                Some(json!({"mode": "bypass"})),
                None,
            ),
            &[(
                ".hook-stop-continue.sh",
                "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"continue\":false,\"stopReason\":\"no more turns\"}'\n",
            )],
        )
        .await;
        harness.set_responses(vec![text_step("first"), text_step("must not run")]);

        harness
            .session
            .prompt("go".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        let texts = message_texts(&harness.session);
        assert!(texts.iter().any(|text| text.contains("first")));
        assert!(
            !texts.iter().any(|text| text.contains("must not run")),
            "continue=false on Stop must prevent the next turn"
        );
        harness.cleanup();
    })
    .await;
}

/// SessionStart `additionalContext` 注入下一次请求。
#[tokio::test]
async fn session_start_additional_context_reaches_the_first_request() {
    run(async {
        use std::sync::{Arc, Mutex};
        let harness = create_session(
            settings_with_hooks(
                json!({
                    "SessionStart": [{
                        "hooks": [{
                            "type": "command",
                            "command": "sh .hook-session.sh"
                        }]
                    }]
                }),
                Some(json!({"mode": "bypass"})),
                None,
            ),
            &[(
                ".hook-session.sh",
                "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"SessionStart\",\"additionalContext\":\"context from session start\"}}'\n",
            )],
        )
        .await;
        harness.session.run_session_start_hook("startup").await;
        let captured = Arc::new(Mutex::new(Vec::new()));
        harness.set_responses(vec![capturing_factory(Arc::clone(&captured), "done")]);

        harness
            .session
            .prompt("hello".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].contains("context from session start"),
            "SessionStart additionalContext must be injected: {}",
            requests[0]
        );
        harness.cleanup();
    })
    .await;
}

/// PostCompact `additionalContext` 注入压缩后的下一次请求。
#[tokio::test]
async fn post_compact_additional_context_reaches_the_next_request() {
    run(async {
        use kiana_agent_core::context_report::CondensationConfig;
        use kiana_ai::types::{
            AssistantContentBlock, Message, TextContent, TextKind, UserContent, UserMessage,
            UserRole,
        };
        use std::sync::{Arc, Mutex};

        let harness = create_session(
            settings_with_hooks(
                json!({
                    "PostCompact": [{
                        "hooks": [{
                            "type": "command",
                            "command": "sh .hook-post-compact.sh"
                        }]
                    }]
                }),
                Some(json!({"mode": "bypass"})),
                None,
            ),
            &[(
                ".hook-post-compact.sh",
                "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"hookSpecificOutput\":{\"hookEventName\":\"PostCompact\",\"additionalContext\":\"context from post compact\"}}'\n",
            )],
        )
        .await;
        harness.settings_manager.apply_overrides(settings_from_json(json!({
            "compaction": { "keepRecentTokens": 1 }
        })));
        harness.append_session_message(AgentMessage::Standard(Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![kiana_ai::types::UserContentBlock::Text(
                TextContent {
                    kind_text: TextKind::Text,
                    text: "message to compact".to_string(),
                    text_signature: None,
                },
            )]),
            timestamp: 1,
        })));
        let mut assistant = faux_assistant_message("assistant response", Default::default());
        assistant.content = vec![AssistantContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "assistant response".to_string(),
            text_signature: None,
        })];
        harness.append_session_message(AgentMessage::Standard(Message::Assistant(assistant)));

        harness.set_responses(vec![faux_text_response("summary text")]);
        harness
            .session
            .compact_with_condensation_options(
                None,
                Some(CondensationConfig {
                    settle_ms: 1,
                    timeout_ms: 2_000,
                    poll_interval_ms: 1,
                    min_saved_tokens: 1,
                }),
            )
            .await
            .expect("compact");

        let captured = Arc::new(Mutex::new(Vec::new()));
        harness.set_responses(vec![capturing_factory(Arc::clone(&captured), "done")]);
        harness
            .session
            .prompt("after compaction".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].contains("context from post compact"),
            "PostCompact additionalContext must be injected: {}",
            requests[0]
        );
        harness.cleanup();
    })
    .await;
}

/// HTTP hook 白名单从 settings 读取：不在白名单内的 URL 在 I/O 前被拒绝。
#[tokio::test]
async fn http_hook_url_allowlist_is_read_from_settings() {
    run(async {
        let harness = create_session(
            json!({
                "hooks": {
                    "PreToolUse": [{
                        "matcher": "bash",
                        "hooks": [{
                            "type": "http",
                            "url": "https://blocked.example/hook"
                        }]
                    }]
                },
                "allowedHttpHookUrls": ["https://allowed.example/*"]
            }),
            &[],
        )
        .await;
        harness.set_responses(vec![tool_call_step("echo hello"), text_step("done")]);

        harness
            .session
            .prompt("run it".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        let warnings = hook_notification_messages(&harness, "warning");
        assert!(
            warnings
                .iter()
                .any(|message| { message.contains("does not match the allowed URL patterns") }),
            "URL allowlist must block the request: {warnings:?}"
        );
        let audit = harness.session.hook_audit_records();
        let record = audit
            .iter()
            .find(|record| record.event == HookEvent::PreToolUse)
            .expect("PreToolUse audit record");
        assert_eq!(record.outcome, kiana_hooks::HookAuditOutcome::SpawnError);
        assert!(
            record
                .warnings
                .iter()
                .any(|warning| warning.contains("does not match the allowed URL patterns")),
            "audit must keep the policy rejection: {record:?}"
        );
        harness.cleanup();
    })
    .await;
}
