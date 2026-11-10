//! 第 5 周：`/skill:<name>` 用户路径的激活、撤销与 TTL（会话级验收）。

mod common;

use common::*;
use indexmap::IndexMap;
use kiana_ai::providers::faux::{FauxAssistantMessageOptions, FauxResponseStep, faux_tool_call};
use kiana_ai::types::StopReason;
use kiana_ext_api::system_prompt::Skill;
use kiana_resources::loader::SkillsResult;
use kiana_session::agent_session::PromptOptions;
use serde_json::json;
use std::sync::Arc;

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

/// 在临时目录写一个带 frontmatter 的 SKILL.md，返回 (SkillsResult, 保活目录)。
fn skill_fixture(name: &str, frontmatter: &str, body: &str) -> (SkillsResult, tempfile::TempDir) {
    let temp_dir = tempfile::tempdir().expect("skill temp dir");
    let skill_dir = temp_dir.path().join(name);
    std::fs::create_dir_all(&skill_dir).expect("skill dir");
    let file_path = skill_dir.join("SKILL.md");
    std::fs::write(&file_path, format!("---\n{frontmatter}\n---\n\n{body}\n"))
        .expect("write skill");
    let path = file_path.to_string_lossy().into_owned();
    (
        SkillsResult {
            skills: vec![Skill {
                name: name.to_string(),
                description: "Fixture skill".to_string(),
                file_path: path.clone(),
                base_dir: skill_dir.to_string_lossy().into_owned(),
                source_info: source_info(&path),
                disable_model_invocation: false,
            }],
            diagnostics: Vec::new(),
        },
        temp_dir,
    )
}

fn bash_echo_step() -> FauxResponseStep {
    let mut args = IndexMap::new();
    args.insert("command".to_string(), json!("echo allowed-by-skill"));
    FauxResponseStep::Message(kiana_ai::providers::faux::faux_assistant_message(
        faux_tool_call("bash", args),
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

/// `/skill:<name>` 创建 SkillActivation；`allowed-tools` 在其存活期间把 Ask 放行。
#[tokio::test]
async fn slash_skill_creates_an_activation_that_allows_tools() {
    run(async {
        let (skills, _skill_dir) = skill_fixture(
            "test",
            "name: test\ndescription: Fixture skill\nallowed-tools:\n  - \"Bash(echo:*)\"",
            "Skill body",
        );
        let harness = create_harness(HarnessOptions {
            skills: Some(skills),
            settings: Some(settings_from_json(
                json!({"permissions": {"mode": "default"}}),
            )),
            ..Default::default()
        })
        .await;
        // 第一次 prompt 只走 `/skill:` 展开与激活。
        harness.set_responses(vec![faux_text_response("activated")]);
        harness
            .session
            .prompt("/skill:test".to_string(), PromptOptions::default())
            .await
            .expect("activate prompt");

        let activations = harness.session.active_skill_activations();
        assert_eq!(activations.len(), 1, "{activations:?}");
        assert_eq!(activations[0].skill_name, "test");
        assert_eq!(activations[0].allowed_tools, vec!["Bash(echo:*)"]);
        assert!(activations[0].expires_at.is_none(), "no TTL configured");

        // 激活期间：bash echo 不再询问，直接执行。
        harness.set_responses(vec![bash_echo_step(), faux_text_response("done")]);
        harness
            .session
            .prompt("run it".to_string(), PromptOptions::default())
            .await
            .expect("tool prompt");
        assert_eq!(
            harness.events_of_type("permission_requested").len(),
            0,
            "allowed-tools must turn the Ask into Allow while active"
        );
        assert!(
            tool_result_text(&harness, "bash").contains("allowed-by-skill"),
            "{}",
            tool_result_text(&harness, "bash")
        );
        harness.cleanup();
    })
    .await;
}

/// 撤销激活后 `allowed-tools` 立即失效，同一请求回到 Ask（无 UI 时拒绝）。
#[tokio::test]
async fn deactivating_a_skill_restores_ask() {
    run(async {
        let (skills, _skill_dir) = skill_fixture(
            "test",
            "name: test\ndescription: Fixture skill\nallowed-tools:\n  - \"Bash(echo:*)\"",
            "Skill body",
        );
        let harness = create_harness(HarnessOptions {
            skills: Some(skills),
            settings: Some(settings_from_json(
                json!({"permissions": {"mode": "default"}}),
            )),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![faux_text_response("activated")]);
        harness
            .session
            .prompt("/skill:test".to_string(), PromptOptions::default())
            .await
            .expect("activate prompt");
        assert!(harness.session.deactivate_skill("test"));
        assert!(harness.session.active_skill_activations().is_empty());

        harness.set_responses(vec![bash_echo_step(), faux_text_response("done")]);
        harness
            .session
            .prompt("run it".to_string(), PromptOptions::default())
            .await
            .expect("tool prompt");
        assert_eq!(
            harness.events_of_type("permission_requested").len(),
            1,
            "after deactivate the permission layer must ask again"
        );
        assert!(
            tool_result_text(&harness, "bash").contains("No interactive UI"),
            "the tool must be blocked: {}",
            tool_result_text(&harness, "bash")
        );
        harness.cleanup();
    })
    .await;
}

/// settings 的技能激活 TTL 写入 `expires_at`；到期后激活失效。
#[tokio::test]
async fn activation_ttl_expires_the_skill() {
    run(async {
        let (skills, _skill_dir) = skill_fixture(
            "test",
            "name: test\ndescription: Fixture skill\nallowed-tools:\n  - \"Bash(echo:*)\"",
            "Skill body",
        );
        let harness = create_harness(HarnessOptions {
            skills: Some(skills),
            settings: Some(settings_from_json(json!({
                "permissions": {"mode": "default"},
                "skillActivationTtlSeconds": 1
            }))),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![faux_text_response("activated")]);
        harness
            .session
            .prompt("/skill:test".to_string(), PromptOptions::default())
            .await
            .expect("activate prompt");
        let activations = harness.session.active_skill_activations();
        assert_eq!(activations.len(), 1);
        assert!(
            activations[0].expires_at.is_some(),
            "settings TTL must set expires_at; raw={:?}",
            harness.settings_manager.get_settings().raw("skills")
        );

        // 等 TTL 到期：激活会被 prune，同一请求回到 Ask。
        tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
        assert!(
            harness.session.active_skill_activations().is_empty(),
            "expired activation must be pruned"
        );

        harness.set_responses(vec![bash_echo_step(), faux_text_response("done")]);
        harness
            .session
            .prompt("run it".to_string(), PromptOptions::default())
            .await
            .expect("tool prompt");
        assert_eq!(harness.events_of_type("permission_requested").len(), 1);
        harness.cleanup();
    })
    .await;
}

/// `user-invocable: false` 的技能不能被 `/skill:` 打开（也不创建激活）。
#[tokio::test]
async fn slash_skill_respects_user_invocable_false() {
    run(async {
        let (skills, _skill_dir) = skill_fixture(
            "hidden",
            "name: hidden\ndescription: Not for users\nuser-invocable: false",
            "Hidden body",
        );
        let harness = create_harness(HarnessOptions {
            skills: Some(skills),
            ..Default::default()
        })
        .await;
        let observed = Arc::new(std::sync::Mutex::new(String::new()));
        let sink = Arc::clone(&observed);
        harness.set_responses(vec![FauxResponseStep::message_factory(move |input| {
            let texts: Vec<String> = input
                .context
                .as_context()
                .messages
                .iter()
                .filter_map(|message| match message {
                    kiana_ai::types::Message::User(user) => match &user.content {
                        kiana_ai::types::UserContent::Text(text) => Some(text.clone()),
                        kiana_ai::types::UserContent::Blocks(blocks) => Some(
                            blocks
                                .iter()
                                .filter_map(|block| match block {
                                    kiana_ai::types::UserContentBlock::Text(text) => {
                                        Some(text.text.clone())
                                    }
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join(""),
                        ),
                    },
                    _ => None,
                })
                .collect();
            *sink.lock().unwrap() = texts.join("\n");
            async move { kiana_ai::providers::faux::faux_assistant_message("ok", Default::default()) }
        })]);
        harness
            .session
            .prompt("/skill:hidden".to_string(), PromptOptions::default())
            .await
            .expect("prompt");
        assert!(
            harness.session.active_skill_activations().is_empty(),
            "a non-user-invocable skill must not activate"
        );
        assert!(
            observed.lock().unwrap().contains("/skill:hidden"),
            "the command must be sent verbatim when it is rejected"
        );
        harness.cleanup();
    })
    .await;
}
