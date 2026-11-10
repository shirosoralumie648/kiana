//! 移植 `packages/coding-agent/test/skills.test.ts`（28 个测试）。
//!
//! 夹具逐字节复制自 `packages/coding-agent/test/fixtures/{skills,skills-collision,
//! empty-agent,empty-cwd}`。

use std::path::{Path, PathBuf};

use kiana_core::diagnostics::{DiagnosticKind, ResourceType};
use kiana_core::source_info::{
    SourceScope, SyntheticSourceInfoOptions, create_synthetic_source_info,
};
use kiana_resources::skills::prompt::{SkillReader, format_skills_for_prompt};
use kiana_resources::skills::{
    LoadSkillsFromDirOptions, LoadSkillsOptions, Skill, load_skills, load_skills_from_dir,
};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn empty_agent_dir() -> PathBuf {
    fixtures_dir().join("empty-agent")
}

fn empty_cwd() -> PathBuf {
    fixtures_dir().join("empty-cwd")
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn create_test_skill(
    name: &str,
    description: &str,
    file_path: &str,
    base_dir: &str,
    disable_model_invocation: bool,
) -> Skill {
    Skill {
        name: name.to_string(),
        description: description.to_string(),
        file_path: file_path.to_string(),
        base_dir: base_dir.to_string(),
        source_info: create_synthetic_source_info(
            file_path,
            SyntheticSourceInfoOptions {
                source: "test".to_string(),
                ..Default::default()
            },
        ),
        disable_model_invocation,
    }
}

fn load_from(subdir: &str) -> kiana_resources::skills::LoadSkillsResult {
    load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: path_string(&fixtures_dir().join("skills").join(subdir)),
        source: "test".to_string(),
    })
}

// ---------------------------------------------------------------------------
// loadSkillsFromDir
// ---------------------------------------------------------------------------

#[test]
fn should_load_a_valid_skill() {
    let result = load_from("valid-skill");
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].name, "valid-skill");
    assert_eq!(
        result.skills[0].description,
        "A valid skill for testing purposes."
    );
    assert_eq!(result.skills[0].source_info.source, "test");
    assert!(result.diagnostics.is_empty());
}

#[test]
fn should_allow_names_that_dont_match_parent_directory() {
    let result = load_from("name-mismatch");
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].name, "different-name");
    assert!(!result.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("does not match parent directory")
    }));
}

#[test]
fn should_warn_when_name_contains_invalid_characters() {
    let result = load_from("invalid-name-chars");
    assert_eq!(result.skills.len(), 1);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("invalid characters"))
    );
}

#[test]
fn should_warn_when_name_exceeds_64_characters() {
    let result = load_from("long-name");
    assert_eq!(result.skills.len(), 1);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("exceeds 64 characters"))
    );
}

#[test]
fn should_warn_and_skip_skill_when_description_is_missing() {
    let result = load_from("missing-description");
    assert!(result.skills.is_empty());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("description is required"))
    );
}

#[test]
fn should_ignore_unknown_frontmatter_fields() {
    let result = load_from("unknown-field");
    assert_eq!(result.skills.len(), 1);
    assert!(result.diagnostics.is_empty());
}

#[test]
fn should_load_nested_skills_recursively() {
    let result = load_from("nested");
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].name, "child-skill");
    assert!(result.diagnostics.is_empty());
}

#[test]
fn should_prefer_a_directorys_root_skill_md_over_nested_skill_md_files() {
    let result = load_from("root-skill-preferred");
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].name, "root-skill-preferred");
    assert_eq!(result.skills[0].description, "Root skill should win.");
    assert!(result.diagnostics.is_empty());
}

#[test]
fn should_skip_files_without_frontmatter() {
    let result = load_from("no-frontmatter");
    assert!(result.skills.is_empty());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("description is required"))
    );
}

#[test]
fn should_warn_and_skip_skill_when_yaml_frontmatter_is_invalid() {
    let result = load_from("invalid-yaml");
    assert!(result.skills.is_empty());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("at line"))
    );
}

#[test]
fn should_preserve_multiline_descriptions_from_yaml() {
    let result = load_from("multiline-description");
    assert_eq!(result.skills.len(), 1);
    assert!(result.skills[0].description.contains('\n'));
    assert!(
        result.skills[0]
            .description
            .contains("This is a multiline description.")
    );
    assert!(result.diagnostics.is_empty());
}

#[test]
fn should_warn_when_name_contains_consecutive_hyphens() {
    let result = load_from("consecutive-hyphens");
    assert_eq!(result.skills.len(), 1);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("consecutive hyphens"))
    );
}

#[test]
fn should_load_all_skills_from_fixture_directory() {
    let result = load_from("");
    assert!(result.skills.len() >= 6);
}

#[test]
fn should_return_empty_for_non_existent_directory() {
    let result = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: "/non/existent/path".to_string(),
        source: "test".to_string(),
    });
    assert!(result.skills.is_empty());
    assert!(result.diagnostics.is_empty());
}

#[test]
fn should_use_parent_directory_name_when_name_not_in_frontmatter() {
    let result = load_from("valid-skill");
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].name, "valid-skill");
}

#[test]
fn should_parse_disable_model_invocation_frontmatter_field() {
    let result = load_from("disable-model-invocation");
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].name, "disable-model-invocation");
    assert!(result.skills[0].disable_model_invocation);
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("unknown frontmatter field"))
    );
}

#[test]
fn should_default_disable_model_invocation_to_false_when_not_specified() {
    let result = load_from("valid-skill");
    assert_eq!(result.skills.len(), 1);
    assert!(!result.skills[0].disable_model_invocation);
}

// ---------------------------------------------------------------------------
// formatSkillsForPrompt
// ---------------------------------------------------------------------------

#[test]
fn should_return_empty_string_for_no_skills() {
    assert_eq!(format_skills_for_prompt(&[], SkillReader::Read), "");
}

#[test]
fn should_format_skills_as_xml() {
    let skills = [create_test_skill(
        "test-skill",
        "A test skill.",
        "/path/to/skill/SKILL.md",
        "/path/to/skill",
        false,
    )];
    let result = format_skills_for_prompt(&skills, SkillReader::Read);
    assert!(result.contains("<available_skills>"));
    assert!(result.contains("</available_skills>"));
    assert!(result.contains("<skill>"));
    assert!(result.contains("<name>test-skill</name>"));
    assert!(result.contains("<description>A test skill.</description>"));
    assert!(result.contains("<location>/path/to/skill/SKILL.md</location>"));
}

#[test]
fn should_include_intro_text_before_xml() {
    let skills = [create_test_skill(
        "test-skill",
        "A test skill.",
        "/path/to/skill/SKILL.md",
        "/path/to/skill",
        false,
    )];
    let result = format_skills_for_prompt(&skills, SkillReader::Read);
    let xml_start = result.find("<available_skills>").expect("xml");
    let intro_text = &result[..xml_start];
    assert!(intro_text.contains("The following skills provide specialized instructions"));
    assert!(intro_text.contains("Use the read tool to load a skill's file"));
}

#[test]
fn should_escape_xml_special_characters() {
    let skills = [create_test_skill(
        "test-skill",
        "A skill with <special> & \"characters\".",
        "/path/to/skill/SKILL.md",
        "/path/to/skill",
        false,
    )];
    let result = format_skills_for_prompt(&skills, SkillReader::Read);
    assert!(result.contains("&lt;special&gt;"));
    assert!(result.contains("&amp;"));
    assert!(result.contains("&quot;characters&quot;"));
}

#[test]
fn should_format_multiple_skills() {
    let skills = [
        create_test_skill(
            "skill-one",
            "First skill.",
            "/path/one/SKILL.md",
            "/path/one",
            false,
        ),
        create_test_skill(
            "skill-two",
            "Second skill.",
            "/path/two/SKILL.md",
            "/path/two",
            false,
        ),
    ];
    let result = format_skills_for_prompt(&skills, SkillReader::Read);
    assert!(result.contains("<name>skill-one</name>"));
    assert!(result.contains("<name>skill-two</name>"));
    assert_eq!(result.matches("<skill>").count(), 2);
}

#[test]
fn should_exclude_skills_with_disable_model_invocation_from_prompt() {
    let skills = [
        create_test_skill(
            "visible-skill",
            "A visible skill.",
            "/path/visible/SKILL.md",
            "/path/visible",
            false,
        ),
        create_test_skill(
            "hidden-skill",
            "A hidden skill.",
            "/path/hidden/SKILL.md",
            "/path/hidden",
            true,
        ),
    ];
    let result = format_skills_for_prompt(&skills, SkillReader::Read);
    assert!(result.contains("<name>visible-skill</name>"));
    assert!(!result.contains("<name>hidden-skill</name>"));
    assert_eq!(result.matches("<skill>").count(), 1);
}

#[test]
fn should_return_empty_string_when_all_skills_have_disable_model_invocation() {
    let skills = [create_test_skill(
        "hidden-skill",
        "A hidden skill.",
        "/path/hidden/SKILL.md",
        "/path/hidden",
        true,
    )];
    assert_eq!(format_skills_for_prompt(&skills, SkillReader::Read), "");
}

// ---------------------------------------------------------------------------
// loadSkills with options
// ---------------------------------------------------------------------------

#[test]
fn should_load_from_explicit_skill_paths() {
    let result = load_skills(LoadSkillsOptions {
        agent_dir: path_string(&empty_agent_dir()),
        cwd: path_string(&empty_cwd()),
        skill_paths: vec![path_string(&fixtures_dir().join("skills/valid-skill"))],
        include_defaults: true,
    });
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].source_info.scope, SourceScope::Temporary);
    assert!(result.diagnostics.is_empty());
}

#[test]
fn should_warn_when_skill_path_does_not_exist() {
    let result = load_skills(LoadSkillsOptions {
        agent_dir: path_string(&empty_agent_dir()),
        cwd: path_string(&empty_cwd()),
        skill_paths: vec!["/non/existent/path".to_string()],
        include_defaults: true,
    });
    assert!(result.skills.is_empty());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("does not exist"))
    );
}

#[test]
fn should_expand_tilde_in_skill_paths() {
    let home = std::env::var("HOME").unwrap_or_default();
    let home_skills_dir = Path::new(&home).join(".kiana/agent/skills");
    let with_tilde = load_skills(LoadSkillsOptions {
        agent_dir: path_string(&empty_agent_dir()),
        cwd: path_string(&empty_cwd()),
        skill_paths: vec!["~/.kiana/agent/skills".to_string()],
        include_defaults: true,
    });
    let without_tilde = load_skills(LoadSkillsOptions {
        agent_dir: path_string(&empty_agent_dir()),
        cwd: path_string(&empty_cwd()),
        skill_paths: vec![path_string(&home_skills_dir)],
        include_defaults: true,
    });
    assert_eq!(with_tilde.skills.len(), without_tilde.skills.len());
}

#[test]
fn should_detect_name_collisions_and_keep_first_skill() {
    let collision_dir = fixtures_dir().join("skills-collision");
    let result = load_skills(LoadSkillsOptions {
        agent_dir: path_string(&empty_agent_dir()),
        cwd: path_string(&empty_cwd()),
        skill_paths: vec![
            path_string(&collision_dir.join("first")),
            path_string(&collision_dir.join("second")),
        ],
        include_defaults: false,
    });
    assert_eq!(result.skills.len(), 1);
    assert_eq!(
        result.skills[0].file_path,
        path_string(&collision_dir.join("first/calendar/SKILL.md"))
    );

    let collision = result
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.kind == DiagnosticKind::Collision)
        .expect("collision diagnostic");
    assert_eq!(collision.message, "name \"calendar\" collision");
    assert_eq!(
        collision.path.as_deref(),
        Some(path_string(&collision_dir.join("second/calendar/SKILL.md")).as_str())
    );
    let details = collision.collision.as_ref().expect("collision details");
    assert_eq!(details.resource_type, ResourceType::Skill);
    assert_eq!(details.name, "calendar");
    assert_eq!(
        details.winner_path,
        path_string(&collision_dir.join("first/calendar/SKILL.md"))
    );
    assert_eq!(
        details.loser_path,
        path_string(&collision_dir.join("second/calendar/SKILL.md"))
    );

    // 碰撞诊断永远排在最后。
    assert_eq!(result.diagnostics.last(), Some(collision));
}

#[test]
fn should_respect_gitignore_rules_relative_to_the_scan_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    std::fs::create_dir_all(root.join("ignored")).expect("ignored dir");
    std::fs::create_dir_all(root.join("kept")).expect("kept dir");
    std::fs::write(
        root.join("ignored/SKILL.md"),
        "---\nname: ignored\ndescription: ignored\n---\n",
    )
    .expect("ignored skill");
    std::fs::write(
        root.join("kept/SKILL.md"),
        "---\nname: kept\ndescription: kept\n---\n",
    )
    .expect("kept skill");
    std::fs::write(root.join(".gitignore"), "ignored/\n").expect("gitignore");

    let result = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: path_string(root),
        source: "test".to_string(),
    });
    assert_eq!(
        result
            .skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>(),
        vec!["kept"]
    );

    // 子目录里的 ignore 文件规则会带上相对 root 的前缀。
    std::fs::write(root.join("kept/.gitignore"), "SKILL.md\n").expect("nested gitignore");
    let result = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: path_string(root),
        source: "test".to_string(),
    });
    assert!(result.skills.is_empty());
}

#[test]
fn formats_the_exact_ts_intro_and_xml_bytes() {
    let skills = [create_test_skill(
        "test-skill",
        "A test skill.",
        "/path/to/skill/SKILL.md",
        "/path/to/skill",
        false,
    )];
    assert_eq!(
        format_skills_for_prompt(&skills, SkillReader::Read),
        concat!(
            "\n\nThe following skills provide specialized instructions for specific tasks.\n",
            "Use the read tool to load a skill's file when the task matches its description.\n",
            "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.\n",
            "\n",
            "<available_skills>\n",
            "  <skill>\n",
            "    <name>test-skill</name>\n",
            "    <description>A test skill.</description>\n",
            "    <location>/path/to/skill/SKILL.md</location>\n",
            "  </skill>\n",
            "</available_skills>"
        )
    );
}

// ---------------------------------------------------------------------------
// readdir 顺序与 npm `ignore` 语义（对 npm ignore 7.0.8 的回归测试）
// ---------------------------------------------------------------------------

fn write_skill_dir(root: &Path, dir_name: &str, skill_name: &str) {
    let dir = root.join(dir_name);
    std::fs::create_dir_all(&dir).expect("skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {skill_name}\ndescription: {skill_name}\n---\n"),
    )
    .expect("skill file");
}

fn loaded_names(root: &Path) -> Vec<String> {
    load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: path_string(root),
        source: "test".to_string(),
    })
    .skills
    .into_iter()
    .map(|skill| skill.name)
    .collect()
}

#[test]
fn should_discover_skills_in_sorted_readdir_order() {
    // libuv 的 uv_fs_scandir 按文件名（strcmp）排序；这里按非字母顺序创建。
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    write_skill_dir(root, "zeta-skill", "zeta-skill");
    write_skill_dir(root, "alpha-skill", "alpha-skill");
    write_skill_dir(root, "mike-skill", "mike-skill");
    assert_eq!(
        loaded_names(root),
        vec!["alpha-skill", "mike-skill", "zeta-skill"]
    );
}

#[test]
fn should_treat_braces_in_ignore_rules_literally() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    write_skill_dir(root, "skill-a", "skill-a");
    write_skill_dir(root, "skill-b", "skill-b");
    write_skill_dir(root, "skill-{a,b}", "skill-{a,b}");
    std::fs::write(root.join(".gitignore"), "skill-{a,b}/\n").expect("gitignore");
    assert_eq!(loaded_names(root), vec!["skill-a", "skill-b"]);
}

#[test]
fn should_not_treat_a_slash_only_ignore_rule_as_match_all() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    write_skill_dir(root, "kept", "kept");
    std::fs::write(root.join(".gitignore"), "//\n").expect("gitignore");
    assert_eq!(loaded_names(root), vec!["kept"]);
}

#[test]
fn should_expand_posix_classes_in_ignore_rules() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    write_skill_dir(root, "ab", "ab");
    write_skill_dir(root, "a1", "a1");
    std::fs::write(root.join(".gitignore"), "a[[:alpha:]]\n").expect("gitignore");
    assert_eq!(loaded_names(root), vec!["a1"]);
}

#[test]
fn should_match_nothing_for_unterminated_bracket_ignore_rules() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    write_skill_dir(root, "a[b", "a[b");
    write_skill_dir(root, "ab", "ab");
    std::fs::write(root.join(".gitignore"), "a[b\n").expect("gitignore");
    assert_eq!(loaded_names(root), vec!["a[b", "ab"]);
}

// ---------------------------------------------------------------------------
// 回归：node:path 语义与 ignore 的转义 `!`（对拍 TS 参考实现）
// ---------------------------------------------------------------------------

fn write_skill_at(root: &Path, relative_dir: &str, name: &str) {
    let dir = root.join(relative_dir);
    std::fs::create_dir_all(&dir).expect("skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {name} desc\n---\n"),
    )
    .expect("skill file");
}

#[test]
fn escaped_bang_ignore_rules_keep_the_literal_bang() {
    // 根规则 `\!x/` → TS 的 prefixIgnorePattern 只去掉反斜杠，得到否定规则 `!x/`，
    // 因此 x/ 被重新包含（加载）。
    let temp = tempfile::tempdir().expect("tempdir");
    write_skill_at(temp.path(), "x", "x");
    std::fs::write(temp.path().join(".gitignore"), "\\!x/\n").expect("gitignore");
    let result = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: path_string(temp.path()),
        source: "test".to_string(),
    });
    assert_eq!(
        result
            .skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>(),
        vec!["x"]
    );

    // 嵌套规则 `\!` → `sub/!`（匹配不到任何路径），整个子目录仍会被扫描。
    let temp = tempfile::tempdir().expect("tempdir");
    write_skill_at(temp.path(), "x", "x");
    write_skill_at(temp.path(), "sub/y", "y");
    std::fs::write(temp.path().join(".gitignore"), "\\!x/\n").expect("gitignore");
    std::fs::write(temp.path().join("sub/.gitignore"), "\\!\n").expect("nested gitignore");
    let result = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: path_string(temp.path()),
        source: "test".to_string(),
    });
    assert_eq!(
        result
            .skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>(),
        vec!["y", "x"]
    );

    // `\!foo` 匹配字面量 `!foo`（否定），不会误伤 `foo`。
    let temp = tempfile::tempdir().expect("tempdir");
    write_skill_at(temp.path(), "!foo", "bang");
    write_skill_at(temp.path(), "foo", "plain");
    std::fs::write(temp.path().join(".gitignore"), "\\!foo\n").expect("gitignore");
    let result = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir: path_string(temp.path()),
        source: "test".to_string(),
    });
    assert_eq!(
        result
            .skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>(),
        vec!["bang", "plain"]
    );
}

#[test]
fn load_skills_from_dir_uses_node_path_normalization() {
    // `join("<abs>/./sub", "SKILL.md")` 在 TS 里是 `"<abs>/sub/SKILL.md"`。
    let temp = tempfile::tempdir().expect("tempdir");
    write_skill_at(temp.path(), "sub", "subskill");
    let dir = format!("{}/./sub", path_string(temp.path()));
    let result = load_skills_from_dir(LoadSkillsFromDirOptions {
        dir,
        source: "test".to_string(),
    });
    assert_eq!(result.skills.len(), 1);
    let skill = &result.skills[0];
    assert_eq!(skill.name, "subskill");
    assert_eq!(
        skill.file_path,
        format!("{}/sub/SKILL.md", path_string(temp.path()))
    );
    assert_eq!(skill.base_dir, format!("{}/sub", path_string(temp.path())));
    assert_eq!(skill.source_info.path, skill.file_path);
    assert_eq!(
        skill.source_info.base_dir.as_deref(),
        Some(skill.base_dir.as_str())
    );
    assert!(result.diagnostics.is_empty());
}
