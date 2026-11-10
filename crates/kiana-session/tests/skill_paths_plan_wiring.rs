//! 第 5/6 周：`paths` 技能可见性使用**会话 cwd**；plan 状态恢复与 pruning 保护接线。

mod common;

use common::*;
use indexmap::IndexMap;
use kiana_ai::providers::faux::{FauxAssistantMessageOptions, FauxResponseStep, faux_tool_call};
use kiana_ai::types::StopReason;
use kiana_ext_api::system_prompt::Skill;
use kiana_resources::loader::SkillsResult;
use kiana_session::agent_session::{PromptOptions, SessionPlanState};
use kiana_session_core::session_manager::SessionManager;
use serde_json::json;

fn source_info(path: &str) -> kiana_core::source_info::SourceInfo {
    kiana_core::source_info::create_synthetic_source_info(
        path,
        kiana_core::source_info::SyntheticSourceInfoOptions {
            source: "local".to_string(),
            scope: Some(kiana_core::source_info::SourceScope::Project),
            origin: None,
            base_dir: None,
        },
    )
}

fn read_step(path: &str) -> FauxResponseStep {
    let mut args = IndexMap::new();
    args.insert("path".to_string(), json!(path));
    FauxResponseStep::Message(kiana_ai::providers::faux::faux_assistant_message(
        faux_tool_call("read", args),
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        },
    ))
}

/// `paths` 匹配相对会话 cwd 解析：未触达前技能对模型隐藏，触达后出现在系统提示里。
///
/// 进程 cwd 是 workspace 根，与临时会话 cwd 不同；若实现误用进程 cwd，触达后的 glob
/// 不可能命中，测试会失败。
#[tokio::test]
async fn paths_visibility_uses_the_session_cwd() {
    run(async {
        let skill_dir = tempfile::tempdir().expect("skill dir");
        let file_path = skill_dir.path().join("path-skill").join("SKILL.md");
        std::fs::create_dir_all(file_path.parent().unwrap()).expect("dir");
        std::fs::write(
            &file_path,
            "---\nname: path-skill\ndescription: Path scoped skill\npaths:\n  - \"src/**\"\n---\n\nScoped body\n",
        )
        .expect("skill");
        let file_path = file_path.to_string_lossy().into_owned();
        let skills = SkillsResult {
            skills: vec![Skill {
                name: "path-skill".to_string(),
                description: "Path scoped skill".to_string(),
                file_path: file_path.clone(),
                base_dir: std::path::Path::new(&file_path)
                    .parent()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                source_info: source_info(&file_path),
                disable_model_invocation: false,
            }],
            diagnostics: Vec::new(),
        };
        let harness = create_harness(HarnessOptions {
            skills: Some(skills),
            settings: Some(settings_from_json(
                json!({"permissions": {"mode": "bypass"}}),
            )),
            ..Default::default()
        })
        .await;
        let cwd = harness.temp_dir.path();
        std::fs::create_dir_all(cwd.join("src")).expect("src dir");
        std::fs::write(cwd.join("src").join("a.txt"), "hello").expect("file");

        // 未触达路径：系统提示里没有该技能。
        harness.set_responses(vec![faux_text_response("idle")]);
        harness
            .session
            .prompt("hi".to_string(), PromptOptions::default())
            .await
            .expect("idle prompt");
        assert!(
            !harness.session.system_prompt().contains("path-skill"),
            "paths-scoped skill must stay hidden before touching the path"
        );

        // 触达 `src/a.txt`（会话 cwd 下的相对路径）。
        harness.set_responses(vec![read_step("src/a.txt"), faux_text_response("read")]);
        harness
            .session
            .prompt("read it".to_string(), PromptOptions::default())
            .await
            .expect("read prompt");

        let read_text = get_message_text(&kiana_agent_core::types::AgentMessage::Standard(
            kiana_ai::types::Message::ToolResult(get_tool_result(&harness, "read")),
        ));
        assert!(read_text.contains("hello"), "read failed: {read_text}");

        // 下一轮发给模型的 system prompt 应包含技能（agent transcript 里的 system 消息）。
        harness.set_responses(vec![faux_text_response("after")]);
        harness
            .session
            .prompt("again".to_string(), PromptOptions::default())
            .await
            .expect("second prompt");
        let prompt = harness.agent.system_prompt();
        assert!(
            prompt.contains("path-skill"),
            "the touched path must make the skill visible; prompt={prompt}"
        );
        harness.cleanup();
    })
    .await;
}

/// plan-mode 扩展写入的 custom entry 在会话启动时恢复为 plan 状态。
#[tokio::test]
async fn plan_state_is_recovered_from_custom_entries() {
    run(async {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let cwd = temp_dir.path().to_string_lossy().into_owned();
        let mut manager = SessionManager::in_memory(&cwd, None, None).expect("session");
        manager
            .append_custom_entry(
                "plan-mode",
                Some(json!({
                    "enabled": true,
                    "approved": true,
                    "path": "plan.md",
                    "content": "# The plan"
                })),
            )
            .expect("append plan entry");
        let harness = create_harness(HarnessOptions {
            session_manager: Some(manager),
            ..Default::default()
        })
        .await;
        let state = harness.session.plan_state();
        assert_eq!(state.path.as_deref(), Some("plan.md"));
        assert_eq!(state.content.as_deref(), Some("# The plan"));
        assert!(state.plan_mode);
        assert!(state.approved);
        harness.cleanup();
    })
    .await;
}

/// active plan 的 read 结果受聚合预算保护；没有 plan 时同一结果会被清除。
#[tokio::test]
async fn active_plan_reads_are_protected_from_the_aggregate_budget() {
    async fn run_case(with_plan: bool) -> bool {
        let harness = create_harness(HarnessOptions {
            settings: Some(settings_from_json(
                json!({"permissions": {"mode": "bypass"}}),
            )),
            ..Default::default()
        })
        .await;
        if with_plan {
            harness.session.set_plan_state(SessionPlanState {
                path: Some("plan.md".to_string()),
                content: Some("# Plan".to_string()),
                plan_mode: true,
                approved: false,
            });
        }
        // assistant(tool_call read plan.md) + 巨大的 tool_result。
        let mut args = IndexMap::new();
        args.insert("path".to_string(), json!("plan.md"));
        let assistant = kiana_ai::types::AssistantMessage {
            content: vec![kiana_ai::types::AssistantContentBlock::ToolCall(
                kiana_ai::types::ToolCall {
                    kind_tool_call: kiana_ai::types::ToolCallKind::ToolCall,
                    id: "call_plan".to_string(),
                    name: "read".to_string(),
                    arguments: args,
                    thought_signature: None,
                    namespace: None,
                    arguments_error: None,
                },
            )],
            stop_reason: StopReason::ToolUse,
            ..Default::default()
        };
        harness.append_session_message(kiana_agent_core::types::AgentMessage::Standard(
            kiana_ai::types::Message::Assistant(assistant),
        ));
        harness.append_session_message(kiana_agent_core::types::AgentMessage::Standard(
            kiana_ai::types::Message::ToolResult(kiana_ai::types::ToolResultMessage {
                role: kiana_ai::types::ToolResultRole::ToolResult,
                tool_call_id: "call_plan".to_string(),
                tool_name: "read".to_string(),
                content: vec![kiana_ai::types::UserContentBlock::Text(
                    kiana_ai::types::TextContent {
                        kind_text: kiana_ai::types::TextKind::Text,
                        text: "x".repeat(400_000),
                        text_signature: None,
                    },
                )],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: 0,
                duration_ms: None,
            }),
        ));
        harness.set_responses(vec![faux_text_response("done")]);
        harness
            .session
            .prompt("continue".to_string(), PromptOptions::default())
            .await
            .expect("prompt");
        let cleared = harness.session_entries().iter().any(|entry| {
            serde_json::to_string(entry)
                .map(|json| json.contains("Old tool result content cleared"))
                .unwrap_or(false)
        });
        harness.cleanup();
        cleared
    }

    run(async {
        assert!(
            !run_case(true).await,
            "an active plan read must not be cleared by the aggregate budget"
        );
        assert!(
            run_case(false).await,
            "the same oversized result without a plan must be cleared (control)"
        );
    })
    .await;
}
