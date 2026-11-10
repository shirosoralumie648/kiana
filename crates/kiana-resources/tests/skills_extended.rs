//! 第 5 周技能扩展：frontmatter、条件可见性、预算、激活与 `skill` 工具。

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use kiana_core::source_info::{
    SourceScope, SyntheticSourceInfoOptions, create_synthetic_source_info,
};
use kiana_permissions::PermissionMode;
use kiana_permissions::{
    PermissionDecision, PermissionEngine, PermissionPolicy, PermissionRequest, PermissionRule,
    RuleDecision,
};
use kiana_resources::skills::activation::{
    SkillActivationSet, evaluate_with_activations, parse_allowed_tools,
};
use kiana_resources::skills::definition::{
    SkillArgument, SkillContext, SkillDefinition, SkillMetadata, list_skill_directory,
    load_skill_definition_from_file, paths_match, skill_visible_for_model,
};
use kiana_resources::skills::prompt::{
    SkillReader, format_skill_definitions_for_prompt, format_skills_for_prompt,
    skill_index_budget_chars,
};
use kiana_resources::skills::tool::{SkillTool, create_skill_tool_definition};
use kiana_resources::skills::{Skill, SkillActivation};

fn skill(name: &str, description: &str, source: &str, scope: SourceScope) -> Skill {
    Skill {
        name: name.to_string(),
        description: description.to_string(),
        file_path: format!("/skills/{name}/SKILL.md"),
        base_dir: format!("/skills/{name}"),
        source_info: create_synthetic_source_info(
            &format!("/skills/{name}/SKILL.md"),
            SyntheticSourceInfoOptions {
                source: source.to_string(),
                scope: Some(scope),
                ..Default::default()
            },
        ),
        disable_model_invocation: false,
    }
}

fn definition(name: &str, description: &str, metadata: SkillMetadata) -> SkillDefinition {
    SkillDefinition {
        skill: skill(name, description, "local", SourceScope::User),
        metadata,
        content: format!("body of {name}"),
    }
}

fn write_skill(dir: &std::path::Path, frontmatter: &str, body: &str) {
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(dir.join("scripts/run.sh"), "#!/bin/sh\necho hi\n").unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\n{frontmatter}\n---\n{body}"),
    )
    .unwrap();
}

#[test]
fn parses_extended_frontmatter_and_keeps_the_body() {
    let dir = tempfile::tempdir().unwrap();
    let skill_dir = dir.path().join("builder");
    write_skill(
        &skill_dir,
        r#"name: builder
description: Build things.
allowed-tools:
  - "Bash(git status)"
  - "Read(src/**)"
model: opus
effort: high
context: fork
agent: reviewer
hooks:
  PreToolUse:
    - matcher: Bash
      hooks: [{type: command, command: echo hi}]
paths: src/**/*.rs
user-invocable: false
arguments:
  - name: topic
    required: true
    description: What to build
argument-hint: "<topic>"
"#,
        "\n# Builder\n\nBuild `$ARGUMENTS` now.\n",
    );
    let file = skill_dir.join("SKILL.md").to_string_lossy().into_owned();
    let (definition, diagnostics) = load_skill_definition_from_file(&file, "test");
    let definition = definition.expect("valid skill");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(definition.name(), "builder");
    assert_eq!(definition.skill.description, "Build things.");
    assert_eq!(
        definition.metadata.allowed_tools,
        vec!["Bash(git status)", "Read(src/**)"]
    );
    assert_eq!(definition.metadata.model.as_deref(), Some("opus"));
    assert_eq!(definition.metadata.effort.as_deref(), Some("high"));
    assert_eq!(definition.metadata.context, SkillContext::Fork);
    assert_eq!(definition.metadata.agent.as_deref(), Some("reviewer"));
    assert!(definition.metadata.hooks.is_some());
    assert_eq!(definition.metadata.paths, vec!["src/**/*.rs"]);
    assert!(!definition.metadata.user_invocable);
    assert_eq!(
        definition.metadata.argument_hint.as_deref(),
        Some("<topic>")
    );
    assert_eq!(
        definition.metadata.arguments,
        vec![SkillArgument {
            name: "topic".to_string(),
            required: true,
            description: Some("What to build".to_string()),
        }]
    );
    assert!(definition.content.contains("Build `$ARGUMENTS` now."));
    assert!(!definition.content.contains("---"));
}

#[test]
fn paths_gate_model_visibility_until_a_matching_file_is_touched() {
    let metadata = SkillMetadata {
        paths: vec!["src/**/*.rs".to_string()],
        ..SkillMetadata::default()
    };
    let definition = definition("rust-helper", "Rust helpers.", metadata);
    assert!(!skill_visible_for_model(&definition, &[]));
    assert!(!skill_visible_for_model(
        &definition,
        &[PathBuf::from("docs/readme.md")]
    ));
    assert!(skill_visible_for_model(
        &definition,
        &[PathBuf::from("src/lib.rs")]
    ));
    assert!(paths_match(
        &["src/**/*.rs".to_string()],
        &[PathBuf::from("src/deep/mod.rs")]
    ));
}

#[test]
fn disable_model_invocation_hides_skill_regardless_of_paths() {
    let mut definition = definition("hidden", "Hidden.", SkillMetadata::default());
    definition.metadata.disable_model_invocation = true;
    assert!(!skill_visible_for_model(&definition, &[]));
}

#[test]
fn budget_scales_with_context_window_and_defaults_to_8000() {
    assert_eq!(skill_index_budget_chars(None), 8_000);
    assert_eq!(skill_index_budget_chars(Some(200_000)), 8_000);
    assert_eq!(skill_index_budget_chars(Some(100_000)), 4_000);
}

#[test]
fn budget_truncation_is_deterministic_and_keeps_bundled_full() {
    let long = "x".repeat(400);
    let mut bundled = definition("bundled-skill", &long, SkillMetadata::default());
    bundled.skill.source_info = create_synthetic_source_info(
        "builtin:bundled-skill",
        SyntheticSourceInfoOptions {
            source: "builtin".to_string(),
            scope: Some(SourceScope::Temporary),
            ..Default::default()
        },
    );
    let normal: Vec<SkillDefinition> = (0..6)
        .map(|index| definition(&format!("skill{index}"), &long, SkillMetadata::default()))
        .collect();
    let mut definitions = normal;
    definitions.push(bundled);

    let first = format_skill_definitions_for_prompt(&definitions, SkillReader::Read, 1_200, &[]);
    let second = format_skill_definitions_for_prompt(&definitions, SkillReader::Read, 1_200, &[]);
    assert_eq!(first, second, "truncation must be deterministic");
    assert!(
        first.chars().count() <= 1_200,
        "budget exceeded: {} chars",
        first.chars().count()
    );
    assert!(
        first.contains(&long),
        "bundled description must not be truncated"
    );
    // 非 bundled 至少保留 name/location（可能只剩名字）。
    assert!(first.contains("<name>skill0</name>"));
}

#[test]
fn legacy_skill_formatter_still_matches_existing_contract() {
    let result = format_skills_for_prompt(
        &[skill(
            "test-skill",
            "A test skill.",
            "test",
            SourceScope::User,
        )],
        SkillReader::Read,
    );
    assert!(result.contains("    <description>A test skill.</description>"));
    assert!(result.contains("    <location>/skills/test-skill/SKILL.md</location>"));
}

#[test]
fn allowed_tools_parse_into_precise_permission_rules() {
    let parsed = parse_allowed_tools(&[
        "Bash(git status)".to_string(),
        "Bash(rm -rf build:*)".to_string(),
        "Read(src/**)".to_string(),
        "mcp__docs__search".to_string(),
        "Bash(broken".to_string(),
    ]);
    assert_eq!(parsed.rules.len(), 4);
    assert_eq!(parsed.invalid, vec!["Bash(broken"]);

    let engine = PermissionEngine::new(PermissionPolicy::new(PermissionMode::Default, "."));
    let request = PermissionRequest::from_tool_call(
        "bash",
        &serde_json::json!({"command": "git status"}),
        ".",
    );
    let now = Utc::now();
    let activations = SkillActivationSet::new();
    assert!(engine.evaluate(&request).is_ask());
    let behavior = evaluate_with_activations(&engine, &activations, &request, now);
    assert!(behavior.is_ask());

    let activation = SkillActivation {
        skill_name: "git".to_string(),
        permission_rules: parsed.rules.clone(),
        allowed_tools: vec!["Bash(git status)".to_string()],
        invalid_allowed_tools: Vec::new(),
        hooks: None,
        agent_scope: None,
        expires_at: None,
        context: SkillContext::Inline,
        model: None,
        effort: None,
    };
    activations.activate(activation);
    let allowed = evaluate_with_activations(&engine, &activations, &request, now);
    assert!(
        matches!(allowed, PermissionDecision::Allow { .. }),
        "{allowed:?}"
    );

    // deny 永远优先。
    let deny_engine = PermissionEngine::new(
        PermissionPolicy::new(PermissionMode::Default, ".").with_rule(PermissionRule::command(
            vec!["git".to_string(), "status".to_string()],
            RuleDecision::Deny,
        )),
    );
    let denied = evaluate_with_activations(&deny_engine, &activations, &request, now);
    assert!(
        matches!(denied, PermissionDecision::Deny { .. }),
        "{denied:?}"
    );

    // 激活结束后恢复 Ask。
    assert!(activations.revoke("git"));
    let after = evaluate_with_activations(&engine, &activations, &request, now);
    assert!(after.is_ask(), "{after:?}");
}

#[test]
fn expired_activation_no_longer_allows() {
    let engine = PermissionEngine::new(PermissionPolicy::new(PermissionMode::Default, "."));
    let parsed = parse_allowed_tools(&["Bash(git status)".to_string()]);
    let before = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let after = Utc.with_ymd_and_hms(2026, 1, 1, 0, 30, 0).unwrap();
    let activations = SkillActivationSet::new();
    activations.activate(SkillActivation {
        skill_name: "git".to_string(),
        permission_rules: parsed.rules,
        allowed_tools: vec![],
        invalid_allowed_tools: vec![],
        hooks: None,
        agent_scope: None,
        expires_at: Some(before + chrono::Duration::minutes(10)),
        context: SkillContext::Inline,
        model: None,
        effort: None,
    });
    let request = PermissionRequest::from_tool_call(
        "bash",
        &serde_json::json!({"command": "git status"}),
        ".",
    );
    assert!(evaluate_with_activations(&engine, &activations, &request, before).is_allow());
    assert!(evaluate_with_activations(&engine, &activations, &request, after).is_ask());
    assert_eq!(activations.prune_expired(after), 1);
}

#[test]
fn skill_tool_validates_arguments_escapes_body_and_lists_files() {
    let dir = tempfile::tempdir().unwrap();
    let skill_dir = dir.path().join("builder");
    write_skill(
        &skill_dir,
        r#"name: builder
description: Build things.
allowed-tools:
  - "Bash(git status)"
arguments:
  - name: topic
    required: true
"#,
        "\nBuild <script>alert(1)</script> for $topic then $ARGUMENTS.\n",
    );
    let file = skill_dir.join("SKILL.md").to_string_lossy().into_owned();
    let (definition, _) = load_skill_definition_from_file(&file, "test");
    let definition = definition.expect("skill");
    assert_eq!(
        list_skill_directory(skill_dir.as_path()),
        vec!["SKILL.md", "scripts/run.sh"]
    );

    let tool = SkillTool::new(vec![definition]);
    let missing = tool.invoke("builder", None).expect_err("required");
    assert!(
        missing.contains("Missing required argument \"topic\""),
        "{missing}"
    );
    let unknown = tool
        .invoke(
            "builder",
            Some(
                &serde_json::json!({"nope": "x"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .expect_err("unknown");
    assert!(unknown.contains("Unknown argument \"nope\""));
    assert!(tool.invoke("ghost", None).is_err());

    let args = serde_json::json!({"topic": "<MCP>"});
    let outcome = tool.invoke("builder", args.as_object()).expect("invoke");
    assert!(outcome.content.contains("<skill name=\"builder\""));
    assert!(outcome.content.contains("scripts/run.sh"));
    // 正文与参数都被转义，伪造标签无法跳出 skill 边界。
    assert!(!outcome.content.contains("<script>"));
    assert!(outcome.content.contains("&lt;script&gt;"));
    assert!(outcome.content.contains("&lt;MCP&gt;"));
    assert_eq!(outcome.activation.skill_name, "builder");
    assert_eq!(outcome.activation.permission_rules.len(), 1);
    assert_eq!(tool.active_activations(Utc::now()).len(), 1);

    assert!(tool.deactivate("builder"));
    assert!(tool.active_activations(Utc::now()).is_empty());

    // 会话结束同样撤销。
    tool.invoke("builder", args.as_object())
        .expect("invoke again");
    tool.end_session();
    assert!(tool.active_activations(Utc::now()).is_empty());
}

#[test]
fn skill_tool_rejects_disable_model_invocation_and_builds_a_definition() {
    let mut definition = definition("hidden", "Hidden.", SkillMetadata::default());
    definition.metadata.disable_model_invocation = true;
    let tool = Arc::new(SkillTool::new(vec![definition]));
    let error = tool
        .invoke("hidden", None)
        .expect_err("model invocation disabled");
    assert!(error.contains("disable-model-invocation"));
    let tool_definition = create_skill_tool_definition(tool);
    assert_eq!(tool_definition.name, "skill");
    assert_eq!(
        tool_definition.parameters["required"],
        serde_json::json!(["name"])
    );
}
