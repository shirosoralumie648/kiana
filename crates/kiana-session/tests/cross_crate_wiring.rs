//! 跨 crate 接线的会话级集成测试。
//!
//! 覆盖：
//! - 第 5 周：`skill` 工具按 loader 技能列表注册、系统提示出现技能索引、调用后正文进入上下文；
//! - 第 6 周：手动压缩在摘要失败时使用确定性 fallback（严格更小的结构化摘要）；
//! - 第 9 周：run collector 在真实 AgentEvent 流上产出 chat/model 汇总。

mod common;

use std::sync::Arc;

use common::*;
use indexmap::IndexMap;
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message, faux_tool_call,
};
use kiana_ai::types::{AssistantContentBlock, AssistantMessage, StopReason, TextContent, TextKind};
use kiana_core::source_info::{
    SourceScope, SyntheticSourceInfoOptions, create_synthetic_source_info,
};
use kiana_ext_api::system_prompt::Skill;
use kiana_resources::loader::SkillsResult;
use kiana_session::agent_session::PromptOptions;
use kiana_session_core::session_manager::SessionEntry;
use serde_json::json;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn write_skill(dir: &std::path::Path, name: &str, description: &str) -> Skill {
    let skill_dir = dir.join("skills").join(name);
    std::fs::create_dir_all(&skill_dir).expect("skill dir");
    let file_path = skill_dir.join("SKILL.md");
    std::fs::write(
        &file_path,
        format!(
            "---\nname: {name}\ndescription: {description}\nallowed-tools:\n  - Read\n---\n\nHello from the {name} skill body.\n"
        ),
    )
    .expect("skill file");
    let file_path = file_path.to_string_lossy().into_owned();
    Skill {
        name: name.to_string(),
        description: description.to_string(),
        file_path: file_path.clone(),
        base_dir: skill_dir.to_string_lossy().into_owned(),
        source_info: create_synthetic_source_info(
            &file_path,
            SyntheticSourceInfoOptions {
                source: "local".to_string(),
                scope: Some(SourceScope::Project),
                ..Default::default()
            },
        ),
        disable_model_invocation: false,
    }
}

/// 技能定义非空时注册 `skill` 工具；系统提示出现索引；调用后正文被包进 `<skill>`。
#[tokio::test]
async fn skill_tool_is_registered_and_loads_the_skill_body() {
    run(async {
        let temp = tempfile::tempdir().expect("temp");
        let skill = write_skill(temp.path(), "demo", "Demo skill for wiring tests");
        let harness = create_harness(HarnessOptions {
            skills: Some(SkillsResult {
                skills: vec![skill],
                diagnostics: Vec::new(),
            }),
            ..Default::default()
        })
        .await;

        assert!(
            harness
                .session
                .get_active_tool_names()
                .contains(&"skill".to_string()),
            "skill tool must be active when definitions exist"
        );
        // 系统提示只放 name/description/location，不包含正文。
        let prompt = harness.session.system_prompt();
        assert!(prompt.contains("demo"));
        assert!(prompt.contains("Demo skill for wiring tests"));
        assert!(!prompt.contains("Hello from the demo skill body."));

        let mut arguments = IndexMap::new();
        arguments.insert("name".to_string(), json!("demo"));
        harness.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                faux_tool_call("skill", arguments),
                FauxAssistantMessageOptions {
                    stop_reason: Some(StopReason::ToolUse),
                    ..Default::default()
                },
            )),
            faux_text_response("done"),
        ]);

        harness
            .session
            .prompt("load the demo skill".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        let tool_text = harness
            .session
            .messages()
            .into_iter()
            .find_map(|message| match message {
                kiana_agent_core::types::AgentMessage::Standard(
                    kiana_ai::types::Message::ToolResult(result),
                ) => Some(get_message_text(
                    &kiana_agent_core::types::AgentMessage::Standard(
                        kiana_ai::types::Message::ToolResult(result),
                    ),
                )),
                _ => None,
            })
            .expect("skill tool result");
        assert!(tool_text.contains("<skill name=\"demo\""));
        assert!(tool_text.contains("Hello from the demo skill body."));
        harness.cleanup();
    })
    .await;
}

fn seed_large_history(harness: &Harness) {
    harness
        .settings_manager
        .apply_overrides(settings_from_json(json!({
            "compaction": { "enabled": true, "keepRecentTokens": 1, "reserveTokens": 0 }
        })));
    let now = now_ms();
    let user_text = format!("please compact this old context {}", "u".repeat(4000));
    harness.append_session_message(kiana_agent_core::types::AgentMessage::Standard(
        kiana_ai::types::Message::User(kiana_ai::types::UserMessage {
            role: kiana_ai::types::UserRole::User,
            content: kiana_ai::types::UserContent::Text(user_text),
            timestamp: now - 1000,
        }),
    ));
    let assistant = AssistantMessage {
        content: vec![AssistantContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: format!("old assistant answer {}", "a".repeat(4000)),
            text_signature: None,
        })],
        timestamp: now - 500,
        ..Default::default()
    };
    harness.append_session_message(kiana_agent_core::types::AgentMessage::Standard(
        kiana_ai::types::Message::Assistant(assistant),
    ));
}

/// 手动压缩的 LLM 摘要失败时，确定性 fallback 仍提交一个结构化摘要。
#[tokio::test]
async fn manual_compaction_uses_deterministic_fallback_when_summary_fails() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        seed_large_history(&harness);
        harness
            .agent
            .set_stream_function(Arc::new(|_model, _context, _options| {
                Box::pin(async {
                    Err(kiana_ai::AiError::new(
                        kiana_ai::AiErrorKind::Other,
                        "summary generator blew up",
                    ))
                })
            }));

        let result = harness
            .session
            .compact(None)
            .await
            .expect("fallback compaction should succeed");
        assert!(
            result.summary.contains("## Objective"),
            "{}",
            result.summary
        );
        assert!(result.usage.is_none(), "fallback must not claim LLM usage");
        assert!(
            harness
                .session_manager()
                .get_entries()
                .iter()
                .any(|entry| matches!(entry, SessionEntry::Compaction(_)))
        );
        harness.cleanup();
    })
    .await;
}

/// run collector 消费 AgentEvent：chat 与 model 汇总在 prompt 结束后可用。
#[tokio::test]
async fn run_collector_reports_chats_and_models() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![faux_text_response("hello")]);
        harness
            .session
            .prompt("hi".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        let summary = harness.session.run_summary().expect("run summary");
        assert_eq!(summary.chats.total, 1);
        assert_eq!(summary.step_count, 1);
        let coverage = harness.session.run_coverage().expect("run coverage");
        assert!(
            coverage
                .models_used
                .iter()
                .any(|model| model.contains("faux"))
        );
        harness.cleanup();
    })
    .await;
}
