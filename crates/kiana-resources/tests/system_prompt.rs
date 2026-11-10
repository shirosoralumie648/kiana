//! 移植 `packages/coding-agent/test/system-prompt.test.ts`（16 个测试）与
//! `system-prompt-updates.test.ts` 中与 section/diff/preamble 直接相关的断言
//! （SDK 级测试留给 `kiana-session`）。

use indexmap::IndexMap;
use kiana_core::source_info::{SyntheticSourceInfoOptions, create_synthetic_source_info};
use kiana_ext_api::system_prompt::{BuildSystemPromptOptions, ContextFile, Skill};
use kiana_resources::system_prompt::build_system_prompt;

fn cwd() -> String {
    std::env::current_dir()
        .expect("cwd")
        .to_string_lossy()
        .into_owned()
}

fn options() -> BuildSystemPromptOptions {
    BuildSystemPromptOptions {
        cwd: cwd(),
        ..Default::default()
    }
}

fn test_skill() -> Skill {
    Skill {
        name: "test-skill".to_string(),
        description: "A test skill.".to_string(),
        file_path: "/skills/test-skill/SKILL.md".to_string(),
        base_dir: "/skills/test-skill".to_string(),
        source_info: create_synthetic_source_info(
            "/skills/test-skill/SKILL.md",
            SyntheticSourceInfoOptions {
                source: "test".to_string(),
                ..Default::default()
            },
        ),
        disable_model_invocation: false,
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

// ---------------------------------------------------------------------------
// system-prompt.test.ts
// ---------------------------------------------------------------------------

#[test]
fn shows_none_for_empty_tools_list() {
    let prompt = build_system_prompt(&BuildSystemPromptOptions {
        selected_tools: Some(Vec::new()),
        context_files: Some(Vec::new()),
        skills: Some(Vec::new()),
        cwd: cwd(),
        ..Default::default()
    })
    .expect("prompt");
    assert!(prompt.contains("<tools>\n(none)\n"));
}

#[test]
fn shows_file_paths_guideline_even_with_no_tools() {
    let prompt = build_system_prompt(&BuildSystemPromptOptions {
        selected_tools: Some(Vec::new()),
        context_files: Some(Vec::new()),
        skills: Some(Vec::new()),
        cwd: cwd(),
        ..Default::default()
    })
    .expect("prompt");
    assert!(prompt.contains("Show file paths clearly"));
}

#[test]
fn keeps_the_default_and_custom_prompt_prefixes_exact() {
    let default_prompt = build_system_prompt(&BuildSystemPromptOptions {
        cwd: "/tmp".to_string(),
        selected_tools: Some(Vec::new()),
        context_files: Some(Vec::new()),
        skills: Some(Vec::new()),
        ..Default::default()
    })
    .expect("prompt");
    let custom_prompt = build_system_prompt(&BuildSystemPromptOptions {
        custom_prompt: Some("You are Exact.".to_string()),
        cwd: "/tmp".to_string(),
        selected_tools: Some(Vec::new()),
        context_files: Some(Vec::new()),
        skills: Some(Vec::new()),
        ..Default::default()
    })
    .expect("prompt");

    assert!(
        default_prompt.starts_with("You are an expert coding assistant operating inside kiana")
    );
    assert!(custom_prompt.starts_with("You are Exact.\n\n<cwd>"));
}

#[test]
fn preserves_an_exact_forced_prompt_without_sections() {
    assert_eq!(
        build_system_prompt(&BuildSystemPromptOptions {
            force_system_prompt: Some("exact".to_string()),
            cwd: "/tmp".to_string(),
            ..Default::default()
        })
        .expect("prompt"),
        "exact"
    );
}

#[test]
fn maps_appended_instructions_and_project_context_to_stable_sections() {
    let prompt = build_system_prompt(&BuildSystemPromptOptions {
        custom_prompt: Some("You are Exact.".to_string()),
        append_system_prompt: Some("Additional instructions.".to_string()),
        context_files: Some(vec![ContextFile {
            path: "/tmp/AGENTS.md".to_string(),
            content: "Project instructions.".to_string(),
        }]),
        selected_tools: Some(Vec::new()),
        skills: Some(Vec::new()),
        cwd: "/tmp".to_string(),
        ..Default::default()
    })
    .expect("prompt");

    assert!(prompt.contains("<addendum>\nAdditional instructions.\n</addendum>"));
    assert!(prompt.contains(
        "<project_context>\nProject-specific instructions and guidelines:\n\n<project_instructions path=\"/tmp/AGENTS.md\">"
    ));
    assert!(prompt.contains("<cwd>\n/tmp\n</cwd>"));
}

#[test]
fn includes_all_default_tools_when_snippets_are_provided() {
    let mut tool_snippets = IndexMap::new();
    tool_snippets.insert("read".to_string(), "Read file contents".to_string());
    tool_snippets.insert("bash".to_string(), "Execute bash commands".to_string());
    tool_snippets.insert("edit".to_string(), "Make surgical edits".to_string());
    tool_snippets.insert("write".to_string(), "Create or overwrite files".to_string());
    let prompt = build_system_prompt(&BuildSystemPromptOptions {
        tool_snippets: Some(tool_snippets),
        context_files: Some(Vec::new()),
        skills: Some(Vec::new()),
        cwd: cwd(),
        ..Default::default()
    })
    .expect("prompt");

    assert!(prompt.contains("- read:"));
    assert!(prompt.contains("- bash:"));
    assert!(prompt.contains("- edit:"));
    assert!(prompt.contains("- write:"));
}

#[test]
fn uses_shell_specific_guidance() {
    for (selected, expected) in [
        (
            vec!["powershell".to_string()],
            "Use PowerShell for file operations",
        ),
        (
            strings(&["bash", "powershell"]),
            "Use bash or PowerShell for file operations",
        ),
    ] {
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            selected_tools: Some(selected),
            context_files: Some(Vec::new()),
            skills: Some(Vec::new()),
            ..options()
        })
        .expect("prompt");
        assert!(prompt.contains(expected));
    }
}

#[test]
fn instructs_models_to_resolve_kiana_docs_and_examples_under_absolute_base_paths() {
    let prompt = build_system_prompt(&BuildSystemPromptOptions {
        context_files: Some(Vec::new()),
        skills: Some(Vec::new()),
        ..options()
    })
    .expect("prompt");
    assert!(prompt.contains(
        "- When reading kiana docs or examples, resolve docs/... under Additional docs and examples/... under Examples, not the current working directory"
    ));
    assert!(prompt.contains(
        "environment variables (docs/environment-variables.md), MCP servers (docs/mcp.md)"
    ));
}

#[test]
fn includes_custom_tools_when_prompt_snippet_is_provided() {
    let mut tool_snippets = IndexMap::new();
    tool_snippets.insert(
        "dynamic_tool".to_string(),
        "Run dynamic test behavior".to_string(),
    );
    let prompt = build_system_prompt(&BuildSystemPromptOptions {
        selected_tools: Some(strings(&["read", "dynamic_tool"])),
        tool_snippets: Some(tool_snippets),
        context_files: Some(Vec::new()),
        skills: Some(Vec::new()),
        ..options()
    })
    .expect("prompt");
    assert!(prompt.contains("- dynamic_tool: Run dynamic test behavior"));
}

#[test]
fn omits_custom_tools_without_prompt_snippet() {
    let prompt = build_system_prompt(&BuildSystemPromptOptions {
        selected_tools: Some(strings(&["read", "dynamic_tool"])),
        context_files: Some(Vec::new()),
        skills: Some(Vec::new()),
        ..options()
    })
    .expect("prompt");
    assert!(!prompt.contains("dynamic_tool"));
}

#[test]
fn appends_prompt_guidelines_to_default_guidelines() {
    let prompt = build_system_prompt(&BuildSystemPromptOptions {
        selected_tools: Some(strings(&["read", "dynamic_tool"])),
        prompt_guidelines: Some(strings(&["Use dynamic_tool for project summaries."])),
        context_files: Some(Vec::new()),
        skills: Some(Vec::new()),
        ..options()
    })
    .expect("prompt");
    assert!(prompt.contains("- Use dynamic_tool for project summaries."));
}

#[test]
fn deduplicates_and_trims_prompt_guidelines() {
    let prompt = build_system_prompt(&BuildSystemPromptOptions {
        selected_tools: Some(strings(&["read", "dynamic_tool"])),
        prompt_guidelines: Some(strings(&[
            "Use dynamic_tool for summaries.",
            "  Use dynamic_tool for summaries.  ",
            "   ",
        ])),
        context_files: Some(Vec::new()),
        skills: Some(Vec::new()),
        ..options()
    })
    .expect("prompt");
    assert_eq!(
        prompt.matches("- Use dynamic_tool for summaries.").count(),
        1
    );
}

#[test]
fn includes_skills_with_only_bash() {
    for custom_prompt in [None, Some("Custom system prompt".to_string())] {
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            custom_prompt,
            selected_tools: Some(strings(&["bash"])),
            context_files: Some(Vec::new()),
            skills: Some(vec![test_skill()]),
            ..options()
        })
        .expect("prompt");
        assert!(prompt.contains("<skills>"));
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("<name>test-skill</name>"));
        assert!(prompt.contains("Use bash to load a skill's file"));
    }
}

#[test]
fn omits_skills_without_read_or_bash() {
    let prompt = build_system_prompt(&BuildSystemPromptOptions {
        selected_tools: Some(strings(&["write"])),
        context_files: Some(Vec::new()),
        skills: Some(vec![test_skill()]),
        ..options()
    })
    .expect("prompt");
    assert!(!prompt.contains("<available_skills>"));
}

fn hidden_tools_prompt(hidden_tools: &[&str]) -> String {
    let mut tool_snippets = IndexMap::new();
    tool_snippets.insert("read".to_string(), "Read files".to_string());
    tool_snippets.insert("bash".to_string(), "Run commands".to_string());
    tool_snippets.insert("run".to_string(), "Run a task".to_string());
    let mut tool_guidelines = IndexMap::new();
    tool_guidelines.insert("read".to_string(), strings(&["Use read for files."]));
    tool_guidelines.insert("run".to_string(), strings(&["Prefer run."]));
    build_system_prompt(&BuildSystemPromptOptions {
        selected_tools: Some(strings(&["read", "bash", "run"])),
        hidden_tools: Some(strings(hidden_tools)),
        tool_snippets: Some(tool_snippets),
        tool_guidelines: Some(tool_guidelines),
        context_files: Some(Vec::new()),
        skills: Some(vec![test_skill()]),
        ..options()
    })
    .expect("prompt")
}

#[test]
fn leaves_hidden_tools_out_of_the_tool_list_and_rules() {
    let prompt = hidden_tools_prompt(&["read", "bash"]);
    assert!(prompt.contains("<tools>\n- run: Run a task\n"));
    assert!(!prompt.contains("- read: "));
    assert!(!prompt.contains("Use read for files."));
    assert!(!prompt.contains("Use bash for file operations"));
    assert!(prompt.contains("- Prefer run."));
}

#[test]
fn keeps_skills_without_naming_a_hidden_reader() {
    assert!(
        hidden_tools_prompt(&["read", "bash"])
            .contains("\nLoad a skill's file when the task matches its description.")
    );
    assert!(hidden_tools_prompt(&["read"]).contains("Use bash to load a skill's file"));
    assert!(hidden_tools_prompt(&[]).contains("Use the read tool to load a skill's file"));
}
