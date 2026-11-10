//! 移植 `packages/coding-agent/test/system-prompt-updates.test.ts` 中属于本单元的断言
//! （section / diff / preamble）。SDK 级测试留给 `kiana-session`。

use indexmap::IndexMap;
use kiana_ext_api::system_prompt::BuildSystemPromptOptions;
use kiana_resources::system_prompt::{
    build_system_prompt_sections, build_system_prompt_state, diff_system_prompt_sections,
};

fn nullable(sections: &IndexMap<String, String>) -> IndexMap<String, Option<String>> {
    sections
        .iter()
        .map(|(key, value)| (key.clone(), Some(value.clone())))
        .collect()
}

fn map(entries: &[(&str, &str)]) -> IndexMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

#[test]
fn diffs_sections_into_a_patch() {
    let previous = build_system_prompt_sections(&BuildSystemPromptOptions {
        cwd: "/tmp".to_string(),
        sections: Some(map(&[("plan_mode", "Plan only.")])),
        ..Default::default()
    })
    .expect("previous");
    let current = build_system_prompt_sections(&BuildSystemPromptOptions {
        cwd: "/tmp".to_string(),
        sections: Some(map(&[("plan_mode", "Implementation allowed.")])),
        ..Default::default()
    })
    .expect("current");

    let patch = diff_system_prompt_sections(&nullable(&previous), &current).expect("patch");
    assert_eq!(
        patch.get("plan_mode"),
        Some(&Some(
            "<plan_mode>\nImplementation allowed.\n</plan_mode>".to_string()
        ))
    );
    assert!(!patch.contains_key("preamble"));
    assert!(diff_system_prompt_sections(&nullable(&previous), &previous).is_none());

    let without = build_system_prompt_sections(&BuildSystemPromptOptions {
        cwd: "/tmp".to_string(),
        ..Default::default()
    })
    .expect("without");
    let patch = diff_system_prompt_sections(&nullable(&previous), &without).expect("patch");
    assert_eq!(patch.len(), 1);
    assert_eq!(patch.get("plan_mode"), Some(&None));
}

#[test]
fn keeps_the_preamble_untagged_and_replaces_it_like_any_section() {
    let previous = build_system_prompt_sections(&BuildSystemPromptOptions {
        custom_prompt: Some("You are A.".to_string()),
        cwd: "/tmp".to_string(),
        ..Default::default()
    })
    .expect("previous");
    let current = build_system_prompt_sections(&BuildSystemPromptOptions {
        custom_prompt: Some("You are B.".to_string()),
        cwd: "/tmp".to_string(),
        ..Default::default()
    })
    .expect("current");
    assert_eq!(
        previous.get("preamble").map(String::as_str),
        Some("You are A.")
    );
    assert_eq!(
        diff_system_prompt_sections(&nullable(&previous), &current)
            .expect("patch")
            .get("preamble"),
        Some(&Some("You are B.".to_string()))
    );

    let forced = build_system_prompt_state(&BuildSystemPromptOptions {
        force_system_prompt: Some("Exact prompt.".to_string()),
        cwd: "/tmp".to_string(),
        ..Default::default()
    })
    .expect("state");
    assert_eq!(forced.content, "Exact prompt.");
    assert_eq!(forced.sections, None);

    let structured = build_system_prompt_state(&BuildSystemPromptOptions {
        cwd: "/tmp".to_string(),
        ..Default::default()
    })
    .expect("state");
    assert_eq!(structured.content, "");
    assert_eq!(
        structured.sections,
        Some(
            build_system_prompt_sections(&BuildSystemPromptOptions {
                cwd: "/tmp".to_string(),
                ..Default::default()
            })
            .expect("sections")
        )
    );

    let error = build_system_prompt_sections(&BuildSystemPromptOptions {
        cwd: "/tmp".to_string(),
        sections: Some(map(&[("preamble", "x")])),
        ..Default::default()
    })
    .expect_err("invalid preamble");
    assert!(
        error
            .to_string()
            .contains("Invalid system prompt section name")
    );
}

#[test]
fn declares_default_sections_in_order() {
    let sections = build_system_prompt_sections(&BuildSystemPromptOptions {
        cwd: "/tmp".to_string(),
        ..Default::default()
    })
    .expect("sections");
    assert_eq!(
        sections.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["preamble", "tools", "rules", "docs", "cwd"]
    );
    assert_eq!(
        sections.get("preamble").map(String::as_str),
        Some(
            "You are an expert coding assistant operating inside kiana, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files."
        )
    );
    assert_eq!(
        sections.get("cwd").map(String::as_str),
        Some("<cwd>\n/tmp\n</cwd>")
    );
}
