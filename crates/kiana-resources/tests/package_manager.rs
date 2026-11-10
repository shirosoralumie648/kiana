//! `packages/coding-agent/test/package-manager.test.ts` 的 Rust 移植。
//!
//! 描述块与 `it` 标题逐字保留。原本需要用 `vi.spyOn` 覆盖的私有方法，这里用 PATH 注入的
//! 假 npm/git（`tests/fixtures/package/fake-cli.sh`）记录 argv 并断言逐项一致。

#![cfg(unix)]
#![allow(clippy::await_holding_lock)] // TestEnv 在 await 期间持有环境锁以串行化进程环境。

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};

use common::{TestEnv, is_disabled, is_disabled_includes, is_enabled, is_enabled_includes};
use kiana_config::settings::schema::{PackageSource, PackageSourceObject, Settings};
use kiana_resources::package::{
    InstallOptions, InstallScope, PackageManager, ProgressEvent, ProgressEventType,
    ResolveExtensionSourcesOptions,
};

fn text(value: &str) -> PackageSource {
    PackageSource::Text(value.to_string())
}

#[cfg(unix)]
#[test]
fn fake_cli_appends_concurrent_invocations_without_interleaving() {
    let env = TestEnv::new();
    let npm = Path::new(&env.bin_dir).join("npm");
    let mut children = Vec::new();
    for index in 0..16 {
        children.push(
            std::process::Command::new(&npm)
                .args(["view", &format!("pkg-{index}"), "version", "--json"])
                .spawn()
                .expect("spawn fake npm"),
        );
    }
    for mut child in children {
        assert!(child.wait().expect("wait for fake npm").success());
    }
    let calls = env.calls("npm");
    assert_eq!(
        calls.len(),
        16,
        "interleaved appends lose invocations: {calls:?}"
    );
    for index in 0..16 {
        let package = format!("pkg-{index}");
        assert!(
            calls.iter().any(|call| call.args.get(1) == Some(&package)),
            "missing {package}: {calls:?}"
        );
    }
}

// ===========================================================================
// describe("resolve")
// ===========================================================================

#[tokio::test]
async fn should_return_no_package_sourced_paths_when_no_sources_configured() {
    let env = TestEnv::new();
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(result.extensions.is_empty());
    assert!(result.prompts.is_empty());
    assert!(result.themes.is_empty());
    assert!(
        result
            .skills
            .iter()
            .all(|resource| resource.metadata.source == "auto"
                && resource.metadata.origin == kiana_core::source_info::SourceOrigin::TopLevel)
    );
}

#[tokio::test]
async fn should_resolve_local_extension_paths_from_settings() {
    let env = TestEnv::new();
    let extension_path = Path::new(&env.agent_dir)
        .join("extensions")
        .join("my-extension.ts");
    env.write(&extension_path, "export default function() {}");
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_extension_paths(vec!["extensions/my-extension.ts".to_string()]);
    let package_manager = env.manager(&settings_manager);

    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(
        result
            .extensions
            .iter()
            .any(|resource| resource.path == extension_path.to_string_lossy() && resource.enabled)
    );
}

#[tokio::test]
async fn should_resolve_builtin_extensions_with_user_exclusions_and_project_overrides() {
    let env = TestEnv::new();
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager_with(
        &env.root(),
        &settings_manager,
        vec!["mcp".to_string(), "llama.cpp".to_string()],
    );

    let builtins = async || {
        package_manager
            .resolve(None)
            .await
            .expect("resolve")
            .extensions
            .into_iter()
            .map(|resource| {
                (
                    resource.path,
                    resource.enabled,
                    resource.metadata.source,
                    resource.metadata.scope,
                )
            })
            .collect::<Vec<_>>()
    };

    assert_eq!(
        builtins().await,
        vec![
            (
                "builtin:mcp".to_string(),
                true,
                "builtin".to_string(),
                kiana_core::source_info::SourceScope::User
            ),
            (
                "builtin:llama.cpp".to_string(),
                true,
                "builtin".to_string(),
                kiana_core::source_info::SourceScope::User
            ),
        ]
    );

    settings_manager.set_extension_paths(vec!["-builtin:mcp".to_string()]);
    settings_manager
        .set_project_extension_paths(vec![
            "+builtin:mcp".to_string(),
            "-builtin:llama.cpp".to_string(),
        ])
        .expect("set project");
    assert_eq!(
        builtins().await,
        vec![
            (
                "builtin:mcp".to_string(),
                true,
                "builtin".to_string(),
                kiana_core::source_info::SourceScope::Project
            ),
            (
                "builtin:llama.cpp".to_string(),
                false,
                "builtin".to_string(),
                kiana_core::source_info::SourceScope::Project
            ),
        ]
    );

    settings_manager
        .set_project_extension_paths(vec![])
        .expect("clear project");
    assert_eq!(
        builtins().await,
        vec![
            (
                "builtin:mcp".to_string(),
                false,
                "builtin".to_string(),
                kiana_core::source_info::SourceScope::User
            ),
            (
                "builtin:llama.cpp".to_string(),
                true,
                "builtin".to_string(),
                kiana_core::source_info::SourceScope::User
            ),
        ]
    );
}

#[tokio::test]
async fn should_resolve_skill_paths_from_settings() {
    let env = TestEnv::new();
    let skill_file = Path::new(&env.agent_dir)
        .join("skills")
        .join("my-skill")
        .join("SKILL.md");
    env.write(
        &skill_file,
        "---\nname: test-skill\ndescription: A test skill\n---\nContent",
    );
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_skill_paths(vec!["skills".to_string()]);
    let package_manager = env.manager(&settings_manager);

    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(
        result
            .skills
            .iter()
            .any(|resource| resource.path == skill_file.to_string_lossy() && resource.enabled)
    );
}

#[tokio::test]
async fn should_auto_discover_root_markdown_skills_from_kiana_skill_dirs() {
    let env = TestEnv::new();
    let skill_file = Path::new(&env.agent_dir)
        .join("skills")
        .join("single-file.md");
    env.write(
        &skill_file,
        "---\nname: single-file\ndescription: A root markdown skill\n---\nContent",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(
        result
            .skills
            .iter()
            .any(|resource| resource.path == skill_file.to_string_lossy() && resource.enabled)
    );
}

#[tokio::test]
async fn should_resolve_project_paths_relative_to_kiana() {
    let env = TestEnv::new();
    let extension_path = Path::new(&env.root())
        .join(".kiana")
        .join("extensions")
        .join("project-ext.ts");
    env.write(&extension_path, "export default function() {}");
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager
        .set_project_extension_paths(vec!["extensions/project-ext.ts".to_string()])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);

    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(
        result
            .extensions
            .iter()
            .any(|resource| resource.path == extension_path.to_string_lossy() && resource.enabled)
    );
}

#[tokio::test]
async fn should_auto_discover_user_prompts_with_overrides() {
    let env = TestEnv::new();
    let prompt_path = Path::new(&env.agent_dir).join("prompts").join("auto.md");
    env.write(&prompt_path, "Auto prompt");
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_prompt_template_paths(vec!["!prompts/auto.md".to_string()]);
    let package_manager = env.manager(&settings_manager);

    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(
        result
            .prompts
            .iter()
            .any(|resource| resource.path == prompt_path.to_string_lossy() && !resource.enabled)
    );
}

#[tokio::test]
async fn should_resolve_symlinked_user_and_project_resources_once() {
    let env = TestEnv::new();
    let root = env.root();
    env.set_env("HOME", &root);
    let shared = Path::new(&root).join("shared-resources");
    let shared_extensions = shared.join("extensions");
    let shared_skills = shared.join("skills");
    let shared_prompts = shared.join("prompts");
    let shared_themes = shared.join("themes");
    env.write(
        shared_extensions.join("shared.ts"),
        "export default function() {}",
    );
    env.write(
        shared_skills.join("shared-skill").join("SKILL.md"),
        "---\nname: shared-skill\ndescription: Shared skill\n---\nContent",
    );
    env.write(shared_prompts.join("shared.md"), "Shared prompt");
    env.write(
        shared_themes.join("shared.json"),
        "{\"name\":\"shared-theme\"}",
    );

    for (source, target) in [
        (
            &shared_extensions,
            Path::new(&env.agent_dir).join("extensions"),
        ),
        (&shared_skills, Path::new(&env.agent_dir).join("skills")),
        (&shared_prompts, Path::new(&env.agent_dir).join("prompts")),
        (&shared_themes, Path::new(&env.agent_dir).join("themes")),
        (
            &shared_extensions,
            Path::new(&root).join(".kiana").join("extensions"),
        ),
        (
            &shared_skills,
            Path::new(&root).join(".kiana").join("skills"),
        ),
        (
            &shared_prompts,
            Path::new(&root).join(".kiana").join("prompts"),
        ),
        (
            &shared_themes,
            Path::new(&root).join(".kiana").join("themes"),
        ),
    ] {
        std::fs::create_dir_all(target.parent().expect("parent")).expect("mkdir");
        std::os::unix::fs::symlink(source, &target).expect("symlink");
    }

    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert_eq!(result.extensions.len(), 1);
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.prompts.len(), 1);
    assert_eq!(result.themes.len(), 1);
    assert_eq!(
        result.extensions[0].metadata.scope,
        kiana_core::source_info::SourceScope::Project
    );
    assert_eq!(
        result.skills[0].metadata.scope,
        kiana_core::source_info::SourceScope::Project
    );
    assert_eq!(
        result.prompts[0].metadata.scope,
        kiana_core::source_info::SourceScope::Project
    );
    assert_eq!(
        result.themes[0].metadata.scope,
        kiana_core::source_info::SourceScope::Project
    );
}

#[tokio::test]
async fn should_keep_the_alphabetically_first_path_when_symlinks_share_a_target() {
    // Node 的 readdirSync 按文件名排序；canonicalizePath 去重时保留先加入的那个字面量路径。
    let env = TestEnv::new();
    let shared = Path::new(&env.agent_dir).join("shared").join("ext.ts");
    env.write(&shared, "export default function() {}");
    let extensions = Path::new(&env.agent_dir).join("extensions");
    std::fs::create_dir_all(&extensions).expect("mkdir extensions");
    // 故意乱序创建，确认结果不依赖文件系统顺序。
    for name in ["q-link.ts", "b-link.ts", "m-link.ts", "a-link.ts"] {
        std::os::unix::fs::symlink(&shared, extensions.join(name)).expect("symlink");
    }

    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    let auto_extensions: Vec<&str> = result
        .extensions
        .iter()
        .filter(|resource| resource.metadata.source == "auto")
        .map(|resource| resource.path.as_str())
        .collect();
    assert_eq!(auto_extensions.len(), 1);
    assert!(
        auto_extensions[0].ends_with("extensions/a-link.ts"),
        "kept {} — Node keeps the first sorted literal path",
        auto_extensions[0]
    );
}

#[tokio::test]
async fn should_auto_discover_project_prompts_with_overrides() {
    let env = TestEnv::new();
    let prompt_path = Path::new(&env.root())
        .join(".kiana")
        .join("prompts")
        .join("is.md");
    env.write(&prompt_path, "Is prompt");
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager
        .set_project_prompt_template_paths(vec!["!prompts/is.md".to_string()])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);

    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(
        result
            .prompts
            .iter()
            .any(|resource| resource.path == prompt_path.to_string_lossy() && !resource.enabled)
    );
}

#[tokio::test]
async fn should_resolve_directory_with_package_json_kiana_extensions_in_extensions_setting() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("my-extensions-pkg");
    env.write(
        package_dir.join("package.json"),
        "{\"name\":\"my-extensions-pkg\",\"kiana\":{\"extensions\":[\"./extensions/clip.ts\",\"./extensions/cost.ts\"]}}",
    );
    env.write(
        package_dir.join("extensions/clip.ts"),
        "export default function() {}",
    );
    env.write(
        package_dir.join("extensions/cost.ts"),
        "export default function() {}",
    );
    env.write(
        package_dir.join("extensions/helper.ts"),
        "export const x = 1;",
    );

    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_extension_paths(vec![package_dir.to_string_lossy().into_owned()]);
    let package_manager = env.manager(&settings_manager);

    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(result.extensions.iter().any(|resource| {
        resource.path == package_dir.join("extensions/clip.ts").to_string_lossy()
            && resource.enabled
    }));
    assert!(result.extensions.iter().any(|resource| {
        resource.path == package_dir.join("extensions/cost.ts").to_string_lossy()
            && resource.enabled
    }));
    assert!(
        !result
            .extensions
            .iter()
            .any(|resource| resource.path.ends_with("helper.ts"))
    );
}

// ===========================================================================
// describe("auto-discovered skill metadata")
// ===========================================================================

#[tokio::test]
async fn should_use_the_agent_dir_as_basedir_for_user_kiana_agent_skills() {
    let env = TestEnv::new();
    let skill_path = Path::new(&env.agent_dir).join("skills/user-kiana/SKILL.md");
    env.write(
        &skill_path,
        "---\nname: user-kiana\ndescription: user kiana\n---\n",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    let skill = result
        .skills
        .iter()
        .find(|resource| resource.path == skill_path.to_string_lossy())
        .expect("skill");
    assert_eq!(skill.metadata.source, "auto");
    assert_eq!(
        skill.metadata.scope,
        kiana_core::source_info::SourceScope::User
    );
    assert_eq!(
        skill.metadata.base_dir.as_deref(),
        Some(env.agent_dir.as_str())
    );
}

#[tokio::test]
async fn should_use_the_project_kiana_dir_as_basedir_for_project_kiana_skills() {
    let env = TestEnv::new();
    let project_base_dir = Path::new(&env.root()).join(".kiana");
    let skill_path = project_base_dir.join("skills/project-kiana/SKILL.md");
    env.write(
        &skill_path,
        "---\nname: project-kiana\ndescription: project kiana\n---\n",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    let skill = result
        .skills
        .iter()
        .find(|resource| resource.path == skill_path.to_string_lossy())
        .expect("skill");
    assert_eq!(skill.metadata.source, "auto");
    assert_eq!(
        skill.metadata.scope,
        kiana_core::source_info::SourceScope::Project
    );
    assert_eq!(
        skill.metadata.base_dir.as_deref(),
        Some(project_base_dir.to_string_lossy().as_ref())
    );
}

#[tokio::test]
async fn should_use_agents_as_basedir_for_user_agents_skills() {
    let env = TestEnv::new();
    let root = env.root();
    env.set_env("HOME", &root);
    let agents_base_dir = Path::new(&root).join(".agents");
    let skill_path = agents_base_dir.join("skills/user-agents/SKILL.md");
    env.write(
        &skill_path,
        "---\nname: user-agents\ndescription: user agents\n---\n",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    let skill = result
        .skills
        .iter()
        .find(|resource| resource.path == skill_path.to_string_lossy())
        .expect("skill");
    assert_eq!(skill.metadata.source, "auto");
    assert_eq!(
        skill.metadata.scope,
        kiana_core::source_info::SourceScope::User
    );
    assert_eq!(
        skill.metadata.base_dir.as_deref(),
        Some(agents_base_dir.to_string_lossy().as_ref())
    );
}

#[tokio::test]
async fn should_use_each_project_agents_dir_as_basedir_for_project_agents_skills() {
    let env = TestEnv::new();
    let repo_root = Path::new(&env.root()).join("repo");
    let nested_cwd = repo_root.join("packages/feature");
    std::fs::create_dir_all(&nested_cwd).expect("mkdir");
    std::fs::create_dir_all(repo_root.join(".git")).expect("mkdir");

    let repo_agents_base_dir = repo_root.join(".agents");
    let repo_skill = repo_agents_base_dir.join("skills/repo/SKILL.md");
    env.write(&repo_skill, "---\nname: repo\ndescription: repo\n---\n");
    let package_agents_base_dir = repo_root.join("packages/.agents");
    let package_skill = package_agents_base_dir.join("skills/package/SKILL.md");
    env.write(
        &package_skill,
        "---\nname: package\ndescription: package\n---\n",
    );

    let settings_manager = env.settings_manager(Settings::default());
    let package_manager =
        env.manager_with(&nested_cwd.to_string_lossy(), &settings_manager, vec![]);
    let result = package_manager.resolve(None).await.expect("resolve");

    let resolved_repo = result
        .skills
        .iter()
        .find(|resource| resource.path == repo_skill.to_string_lossy())
        .expect("repo skill");
    assert_eq!(resolved_repo.metadata.source, "auto");
    assert_eq!(
        resolved_repo.metadata.scope,
        kiana_core::source_info::SourceScope::Project
    );
    assert_eq!(
        resolved_repo.metadata.base_dir.as_deref(),
        Some(repo_agents_base_dir.to_string_lossy().as_ref())
    );

    let resolved_package = result
        .skills
        .iter()
        .find(|resource| resource.path == package_skill.to_string_lossy())
        .expect("package skill");
    assert_eq!(
        resolved_package.metadata.base_dir.as_deref(),
        Some(package_agents_base_dir.to_string_lossy().as_ref())
    );
}

// ===========================================================================
// describe(".agents/skills auto-discovery")
// ===========================================================================

#[tokio::test]
async fn should_scan_agents_skills_from_cwd_up_to_git_repo_root() {
    let env = TestEnv::new();
    let repo_root = Path::new(&env.root()).join("repo");
    let nested_cwd = repo_root.join("packages/feature");
    std::fs::create_dir_all(&nested_cwd).expect("mkdir");
    std::fs::create_dir_all(repo_root.join(".git")).expect("mkdir");

    let above_repo_skill = Path::new(&env.root()).join(".agents/skills/above-repo/SKILL.md");
    env.write(
        &above_repo_skill,
        "---\nname: above-repo\ndescription: above\n---\n",
    );
    let repo_root_skill = repo_root.join(".agents/skills/repo-root/SKILL.md");
    env.write(
        &repo_root_skill,
        "---\nname: repo-root\ndescription: repo\n---\n",
    );
    let nested_skill = repo_root.join("packages/.agents/skills/nested/SKILL.md");
    env.write(
        &nested_skill,
        "---\nname: nested\ndescription: nested\n---\n",
    );

    let settings_manager = env.settings_manager(Settings::default());
    let package_manager =
        env.manager_with(&nested_cwd.to_string_lossy(), &settings_manager, vec![]);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(is_enabled(&result.skills, "repo-root/SKILL.md"));
    assert!(is_enabled(&result.skills, "nested/SKILL.md"));
    assert!(
        !result
            .skills
            .iter()
            .any(|resource| resource.path == above_repo_skill.to_string_lossy())
    );
}

#[tokio::test]
async fn should_scan_agents_skills_up_to_filesystem_root_when_not_in_a_git_repo() {
    let env = TestEnv::new();
    let non_repo_root = Path::new(&env.root()).join("non-repo");
    let nested_cwd = non_repo_root.join("a/b");
    std::fs::create_dir_all(&nested_cwd).expect("mkdir");
    let root_skill = non_repo_root.join(".agents/skills/root/SKILL.md");
    env.write(&root_skill, "---\nname: root\ndescription: root\n---\n");
    let middle_skill = non_repo_root.join("a/.agents/skills/middle/SKILL.md");
    env.write(
        &middle_skill,
        "---\nname: middle\ndescription: middle\n---\n",
    );

    let settings_manager = env.settings_manager(Settings::default());
    let package_manager =
        env.manager_with(&nested_cwd.to_string_lossy(), &settings_manager, vec![]);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(is_enabled(&result.skills, "root/SKILL.md"));
    assert!(is_enabled(&result.skills, "middle/SKILL.md"));
}

#[tokio::test]
async fn should_ignore_root_markdown_files_in_agents_skills_but_discover_nested_markdown_skills() {
    let env = TestEnv::new();
    let agents_skills_dir = Path::new(&env.root()).join(".agents/skills");
    let root_skill = agents_skills_dir.join("root-file.md");
    let nested_skill = agents_skills_dir.join("nested-skill/SKILL.md");
    let nested_markdown_skill = agents_skills_dir.join("third-party/child-skill.md");
    let deeply_nested = agents_skills_dir.join("third-party/vendor/pack/deep-skill.md");
    env.write(
        &root_skill,
        "---\nname: root-file\ndescription: Root markdown file\n---\n",
    );
    env.write(
        &nested_skill,
        "---\nname: nested-skill\ndescription: Nested skill\n---\n",
    );
    env.write(
        &nested_markdown_skill,
        "---\nname: child-skill\ndescription: Nested markdown skill\n---\n",
    );
    env.write(
        &deeply_nested,
        "---\nname: deep-skill\ndescription: Deep markdown skill\n---\n",
    );
    let work_dir = Path::new(&env.root()).join("work");
    std::fs::create_dir_all(&work_dir).expect("mkdir");
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager_with(&work_dir.to_string_lossy(), &settings_manager, vec![]);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(
        !result
            .skills
            .iter()
            .any(|resource| resource.path == root_skill.to_string_lossy())
    );
    assert!(is_enabled(&result.skills, "nested-skill/SKILL.md"));
    assert!(is_enabled(&result.skills, "child-skill.md"));
    assert!(is_enabled(&result.skills, "deep-skill.md"));
}

#[tokio::test]
async fn should_keep_home_agents_skills_user_scoped_when_cwd_is_under_home_in_a_non_git_directory()
{
    let env = TestEnv::new();
    let root = env.root();
    env.set_env("HOME", &root);
    let cwd = Path::new(&root).join("scratch/nested");
    std::fs::create_dir_all(&cwd).expect("mkdir");
    let local_agent_dir = Path::new(&root).join(".kiana/agent");
    std::fs::create_dir_all(&local_agent_dir).expect("mkdir");
    let home_skill = Path::new(&root).join(".agents/skills/home-skill/SKILL.md");
    env.write(
        &home_skill,
        "---\nname: home-skill\ndescription: home\n---\n",
    );

    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = kiana_resources::package::DefaultPackageManager::new(
        kiana_resources::package::PackageManagerOptions {
            cwd: cwd.to_string_lossy().into_owned(),
            agent_dir: local_agent_dir.to_string_lossy().into_owned(),
            settings_manager: settings_manager.clone(),
            builtin_extensions: vec![],
        },
    );
    let result = package_manager.resolve(None).await.expect("resolve");
    let matches: Vec<_> = result
        .skills
        .iter()
        .filter(|resource| resource.path == home_skill.to_string_lossy())
        .collect();
    assert_eq!(matches.len(), 1);
    assert!(matches[0].enabled);
    assert_eq!(
        matches[0].metadata.scope,
        kiana_core::source_info::SourceScope::User
    );
    assert_eq!(matches[0].metadata.source, "auto");
}

#[tokio::test]
async fn should_dedupe_user_skill_entries_when_kiana_agent_skills_is_a_symlink_to_agents_skills() {
    let env = TestEnv::new();
    let root = env.root();
    env.set_env("HOME", &root);
    let agent_skills_dir = Path::new(&env.agent_dir).join("skills");
    let agents_skills_dir = Path::new(&root).join(".agents/skills");
    std::fs::create_dir_all(&agents_skills_dir).expect("mkdir");
    std::fs::create_dir_all(agent_skills_dir.parent().expect("parent")).expect("mkdir");
    std::os::unix::fs::symlink(&agents_skills_dir, &agent_skills_dir).expect("symlink");
    env.write(
        agents_skills_dir.join("foo/SKILL.md"),
        "---\nname: foo\ndescription: foo\n---\n",
    );

    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    let foo_skills: Vec<_> = result
        .skills
        .iter()
        .filter(|resource| resource.path.ends_with("foo/SKILL.md"))
        .collect();
    assert_eq!(foo_skills.len(), 1);
}

// ===========================================================================
// describe("ignore files")
// ===========================================================================

#[tokio::test]
async fn should_respect_gitignore_in_skill_directories() {
    let env = TestEnv::new();
    let skills_dir = Path::new(&env.agent_dir).join("skills");
    env.write(skills_dir.join(".gitignore"), "venv\n__pycache__\n");
    env.write(
        skills_dir.join("good-skill/SKILL.md"),
        "---\nname: good-skill\ndescription: Good\n---\nContent",
    );
    env.write(
        skills_dir.join("venv/bad-skill/SKILL.md"),
        "---\nname: bad-skill\ndescription: Bad\n---\nContent",
    );
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_skill_paths(vec!["skills".to_string()]);
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(is_enabled_includes(&result.skills, "good-skill"));
    assert!(!is_enabled_includes(&result.skills, "venv"));
}

#[tokio::test]
async fn should_not_apply_parent_gitignore_to_kiana_auto_discovery() {
    let env = TestEnv::new();
    env.write(Path::new(&env.root()).join(".gitignore"), ".kiana\n");
    let skill_path = Path::new(&env.root()).join(".kiana/skills/auto-skill/SKILL.md");
    env.write(
        &skill_path,
        "---\nname: auto-skill\ndescription: Auto\n---\nContent",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(
        result
            .skills
            .iter()
            .any(|resource| resource.path == skill_path.to_string_lossy() && resource.enabled)
    );
}

// ===========================================================================
// describe("resolveExtensionSources")
// ===========================================================================

#[tokio::test]
async fn should_resolve_local_paths() {
    let env = TestEnv::new();
    let extension_path = Path::new(&env.root()).join("ext.ts");
    env.write(&extension_path, "export default function() {}");
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager
        .resolve_extension_sources(
            &[extension_path.to_string_lossy().into_owned()],
            ResolveExtensionSourcesOptions::default(),
        )
        .await
        .expect("resolve");
    assert!(
        result
            .extensions
            .iter()
            .any(|resource| resource.path == extension_path.to_string_lossy() && resource.enabled)
    );
}

#[tokio::test]
async fn should_handle_directories_with_kiana_manifest_from_extension_sources() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("my-package");
    env.write(
        package_dir.join("package.json"),
        "{\"name\":\"my-package\",\"kiana\":{\"extensions\":[\"./src/index.ts\"],\"skills\":[\"./skills\"]}}",
    );
    env.write(
        package_dir.join("src/index.ts"),
        "export default function() {}",
    );
    env.write(
        package_dir.join("skills/my-skill/SKILL.md"),
        "---\nname: my-skill\ndescription: Test\n---\nContent",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager
        .resolve_extension_sources(
            &[package_dir.to_string_lossy().into_owned()],
            ResolveExtensionSourcesOptions::default(),
        )
        .await
        .expect("resolve");
    assert!(
        result
            .extensions
            .iter()
            .any(|resource| resource.path.ends_with("src/index.ts") && resource.enabled)
    );
    assert!(
        result
            .skills
            .iter()
            .any(|resource| resource.path.ends_with("my-skill/SKILL.md") && resource.enabled)
    );
}

#[tokio::test]
async fn should_keep_kiana_manifest_entries_with_leading_tilde_package_relative() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("tilde-manifest-package");
    let direct_extension_path = package_dir.join("~extensions/main.ts");
    let slash_extension_path = package_dir.join("~/extensions/alt.ts");
    let direct_skill_path = package_dir.join("~skills/direct-skill/SKILL.md");
    let slash_skill_path = package_dir.join("~/skills/slash-skill/SKILL.md");
    env.write(&direct_extension_path, "export default function() {}");
    env.write(&slash_extension_path, "export default function() {}");
    env.write(
        &direct_skill_path,
        "---\nname: direct-skill\ndescription: Direct\n---\nContent",
    );
    env.write(
        &slash_skill_path,
        "---\nname: slash-skill\ndescription: Slash\n---\nContent",
    );
    env.write(
        package_dir.join("package.json"),
        "{\"name\":\"tilde-manifest-package\",\"kiana\":{\"extensions\":[\"~extensions/main.ts\",\"~/extensions/alt.ts\"],\"skills\":[\"~skills\",\"~/skills\"]}}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager
        .resolve_extension_sources(
            &[package_dir.to_string_lossy().into_owned()],
            ResolveExtensionSourcesOptions::default(),
        )
        .await
        .expect("resolve");
    assert!(result.extensions.iter().any(|resource| resource.path
        == direct_extension_path.to_string_lossy()
        && resource.enabled));
    assert!(result.extensions.iter().any(|resource| resource.path
        == slash_extension_path.to_string_lossy()
        && resource.enabled));
    assert!(
        result.skills.iter().any(
            |resource| resource.path == direct_skill_path.to_string_lossy() && resource.enabled
        )
    );
    assert!(
        result.skills.iter().any(
            |resource| resource.path == slash_skill_path.to_string_lossy() && resource.enabled
        )
    );
}

#[tokio::test]
async fn should_handle_directories_with_auto_discovery_layout() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("auto-pkg");
    env.write(
        package_dir.join("extensions/main.ts"),
        "export default function() {}",
    );
    env.write(package_dir.join("themes/dark.json"), "{}");
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager
        .resolve_extension_sources(
            &[package_dir.to_string_lossy().into_owned()],
            ResolveExtensionSourcesOptions::default(),
        )
        .await
        .expect("resolve");
    assert!(is_enabled(&result.extensions, "main.ts"));
    assert!(is_enabled(&result.themes, "dark.json"));
}

#[tokio::test]
async fn should_stop_recursing_when_a_package_skill_directory_contains_skill_md() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("skill-root-pkg");
    let root_skill = package_dir.join("skills/root-skill/SKILL.md");
    let nested_skill = package_dir.join("skills/root-skill/nested-skill/SKILL.md");
    env.write(
        &root_skill,
        "---\nname: root-skill\ndescription: Root skill\n---\n",
    );
    env.write(
        &nested_skill,
        "---\nname: nested-skill\ndescription: Nested skill\n---\n",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager
        .resolve_extension_sources(
            &[package_dir.to_string_lossy().into_owned()],
            ResolveExtensionSourcesOptions::default(),
        )
        .await
        .expect("resolve");
    assert!(
        result
            .skills
            .iter()
            .any(|resource| resource.path == root_skill.to_string_lossy() && resource.enabled)
    );
    assert!(
        !result
            .skills
            .iter()
            .any(|resource| resource.path == nested_skill.to_string_lossy())
    );
}

// ===========================================================================
// describe("progress callback")
// ===========================================================================

#[tokio::test]
async fn should_emit_progress_events() {
    let env = TestEnv::new();
    let extension_path = Path::new(&env.root()).join("ext.ts");
    env.write(&extension_path, "export default function() {}");
    let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    package_manager.set_progress_callback(Some(Arc::new(move |event| {
        sink.lock().expect("lock").push(event);
    })));
    package_manager
        .resolve_extension_sources(
            &[extension_path.to_string_lossy().into_owned()],
            ResolveExtensionSourcesOptions::default(),
        )
        .await
        .expect("resolve");
    assert_eq!(events.lock().expect("lock").len(), 0);
}

// ===========================================================================
// describe("command spawning")
// ===========================================================================

#[tokio::test]
async fn should_emit_progress_events_on_install_attempt() {
    let env = TestEnv::new();
    env.set_env("KIANA_FAKE_FAIL_MATCH", "nonexistent-package");
    env.set_env("KIANA_FAKE_FAIL_STDERR", "simulated npm install failure");
    let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    package_manager.set_progress_callback(Some(Arc::new(move |event| {
        sink.lock().expect("lock").push(event);
    })));
    let error = package_manager
        .install("npm:nonexistent-package@1.0.0", InstallOptions::default())
        .await
        .expect_err("install must fail");
    // TS 的 `runCommand` 只带退出码，不带 stderr；测试里 mock 的文案换成真实文案。
    assert!(error.message().contains("npm install"));
    assert!(error.message().contains("failed with code 1"));
    let events = events.lock().expect("lock");
    assert!(
        events
            .iter()
            .any(|event| event.kind == ProgressEventType::Start
                && matches!(
                    event.action,
                    kiana_resources::package::ProgressAction::Install
                ))
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == ProgressEventType::Error)
    );
}

#[tokio::test]
async fn should_recognize_github_urls_without_git_prefix() {
    let env = TestEnv::new();
    let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    package_manager.set_progress_callback(Some(Arc::new(move |event| {
        sink.lock().expect("lock").push(event);
    })));
    let source = "https://github.com/nonexistent/repo";
    package_manager
        .install(source, InstallOptions::default())
        .await
        .expect("install");
    let clone = env.first_call("git").expect("git clone");
    assert_eq!(clone.args[0], "clone");
    assert_eq!(clone.args[1], source);
    assert!(
        events
            .lock()
            .expect("lock")
            .iter()
            .any(|event| event.kind == ProgressEventType::Start)
    );
}

// ===========================================================================
// describe("npmCommand")
// ===========================================================================

fn settings_with_npm_command(command: Vec<&str>) -> Settings {
    Settings {
        npm_command: Some(command.into_iter().map(str::to_string).collect()),
        ..Settings::default()
    }
}

#[tokio::test]
async fn should_use_npm_command_argv_for_npm_installs() {
    let env = TestEnv::new();
    let settings_manager = env.settings_manager(settings_with_npm_command(vec![
        "mise", "exec", "node@20", "--", "npm",
    ]));
    let package_manager = env.manager(&settings_manager);
    package_manager
        .install("npm:@scope/pkg", InstallOptions::default())
        .await
        .expect("install");

    let call = env.first_call("mise").expect("mise invocation");
    let npm_root = std::fs::canonicalize(Path::new(&env.agent_dir).join("npm"))
        .expect("real npm root")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        call.args,
        vec![
            "exec".to_string(),
            "node@20".to_string(),
            "--".to_string(),
            "npm".to_string(),
            "install".to_string(),
            "@scope/pkg".to_string(),
            "--prefix".to_string(),
            npm_root,
            "--legacy-peer-deps".to_string(),
        ]
    );
}

#[tokio::test]
async fn should_pass_legacy_peer_deps_when_uninstalling_npm_packages() {
    let env = TestEnv::new();
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    std::fs::create_dir_all(Path::new(&env.agent_dir).join("npm")).expect("mkdir");
    package_manager
        .remove("npm:@scope/pkg", InstallOptions::default())
        .await
        .expect("remove");
    let call = env.first_call("npm").expect("npm invocation");
    let npm_root = std::fs::canonicalize(Path::new(&env.agent_dir).join("npm"))
        .expect("real npm root")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        call.args,
        vec![
            "uninstall".to_string(),
            "@scope/pkg".to_string(),
            "--prefix".to_string(),
            npm_root,
            "--legacy-peer-deps".to_string(),
        ]
    );
}

#[tokio::test]
async fn should_pass_the_real_path_as_prefix_when_the_agent_dir_is_a_symlink() {
    let env = TestEnv::new();
    let real_agent_dir = Path::new(&env.root()).join("dotfiles/agent");
    std::fs::create_dir_all(&real_agent_dir).expect("mkdir");
    let linked_agent_dir = Path::new(&env.root()).join("linked-agent");
    std::os::unix::fs::symlink(&real_agent_dir, &linked_agent_dir).expect("symlink");
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = kiana_resources::package::DefaultPackageManager::new(
        kiana_resources::package::PackageManagerOptions {
            cwd: env.root(),
            agent_dir: linked_agent_dir.to_string_lossy().into_owned(),
            settings_manager: settings_manager.clone(),
            builtin_extensions: vec![],
        },
    );
    let real_npm_root = std::fs::canonicalize(&real_agent_dir)
        .expect("realpath")
        .join("npm")
        .to_string_lossy()
        .into_owned();

    package_manager
        .install("npm:@scope/pkg", InstallOptions::default())
        .await
        .expect("install");
    package_manager
        .remove("npm:@scope/pkg", InstallOptions::default())
        .await
        .expect("remove");

    let calls = env.calls("npm");
    assert_eq!(
        calls[0].args,
        vec![
            "install".to_string(),
            "@scope/pkg".to_string(),
            "--prefix".to_string(),
            real_npm_root.clone(),
            "--legacy-peer-deps".to_string(),
        ]
    );
    assert_eq!(
        calls[1].args,
        vec![
            "uninstall".to_string(),
            "@scope/pkg".to_string(),
            "--prefix".to_string(),
            real_npm_root,
            "--legacy-peer-deps".to_string(),
        ]
    );
}

#[tokio::test]
async fn should_use_bun_cwd_for_npm_package_installs() {
    let env = TestEnv::new();
    let settings_manager = env.settings_manager(settings_with_npm_command(vec![
        "mise", "exec", "bun@1", "--", "bun",
    ]));
    let package_manager = env.manager(&settings_manager);
    package_manager
        .install("npm:@scope/pkg", InstallOptions::default())
        .await
        .expect("install");
    let call = env.first_call("mise").expect("mise invocation");
    let npm_root = std::fs::canonicalize(Path::new(&env.agent_dir).join("npm"))
        .expect("real npm root")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        call.args,
        vec![
            "exec".to_string(),
            "bun@1".to_string(),
            "--".to_string(),
            "bun".to_string(),
            "install".to_string(),
            "@scope/pkg".to_string(),
            "--cwd".to_string(),
            npm_root,
            "--omit=peer".to_string(),
        ]
    );
}

#[tokio::test]
async fn should_install_git_package_dependencies_without_auto_installing_peers() {
    let env = TestEnv::new();
    env.set_env(
        "KIANA_FAKE_GIT_CLONE_PACKAGE_JSON",
        "{\"name\":\"repo\",\"version\":\"1.0.0\"}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    package_manager
        .install("git:github.com/user/repo", InstallOptions::default())
        .await
        .expect("install");
    let target_dir = Path::new(&env.agent_dir).join("git/github.com/user/repo");
    let call = env.last_call("npm").expect("npm invocation");
    assert_eq!(
        call.args,
        vec![
            "install".to_string(),
            "--omit=dev".to_string(),
            "--legacy-peer-deps".to_string(),
        ]
    );
    assert_eq!(call.cwd, target_dir.to_string_lossy());
}

#[tokio::test]
async fn should_remove_a_newly_created_checkout_when_git_clone_fails() {
    let env = TestEnv::new();
    env.set_env("KIANA_FAKE_GIT_CLONE_FAIL", "1");
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let error = package_manager
        .install("git:github.com/user/repo", InstallOptions::default())
        .await
        .expect_err("install must fail");
    assert!(error.message().contains("git clone"));
    assert!(error.message().contains("failed with code 1"));
    let target_dir = Path::new(&env.agent_dir).join("git/github.com/user/repo");
    assert!(!target_dir.exists());
}

#[tokio::test]
async fn should_remove_a_newly_cloned_checkout_when_dependency_installation_fails() {
    let env = TestEnv::new();
    env.set_env(
        "KIANA_FAKE_GIT_CLONE_PACKAGE_JSON",
        "{\"name\":\"repo\",\"version\":\"1.0.0\"}",
    );
    env.set_env("KIANA_FAKE_FAIL_MATCH", "install");
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let error = package_manager
        .install("git:github.com/user/repo", InstallOptions::default())
        .await
        .expect_err("install must fail");
    assert!(error.message().contains("npm install"));
    assert!(error.message().contains("failed with code 1"));
    let target_dir = Path::new(&env.agent_dir).join("git/github.com/user/repo");
    assert!(!target_dir.exists());
}

#[tokio::test]
async fn should_reconcile_an_existing_git_checkout_to_a_pinned_ref_during_install() {
    let env = TestEnv::new();
    env.set_env("KIANA_FAKE_GIT_LOCAL_HEAD", "old-head");
    env.set_env("KIANA_FAKE_GIT_TARGET_HEAD", "new-head");
    let target_dir = Path::new(&env.agent_dir).join("git/github.com/user/repo");
    env.write(
        target_dir.join("package.json"),
        "{\"name\":\"repo\",\"version\":\"1.0.0\"}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);

    package_manager
        .install("git:github.com/user/repo@v2", InstallOptions::default())
        .await
        .expect("install");

    let git_calls = env.calls("git");
    let command_lines: Vec<Vec<String>> = git_calls.iter().map(|call| call.args.clone()).collect();
    assert!(command_lines.contains(&vec![
        "fetch".to_string(),
        "origin".to_string(),
        "v2".to_string()
    ]));
    assert!(command_lines.contains(&vec![
        "reset".to_string(),
        "--hard".to_string(),
        "FETCH_HEAD^{commit}".to_string()
    ]));
    assert!(command_lines.contains(&vec!["clean".to_string(), "-fdx".to_string()]));
    let npm_call = env.last_call("npm").expect("npm invocation");
    assert_eq!(
        npm_call.args,
        vec![
            "install".to_string(),
            "--omit=dev".to_string(),
            "--legacy-peer-deps".to_string(),
        ]
    );
    assert_eq!(npm_call.cwd, target_dir.to_string_lossy());
}

#[tokio::test]
async fn should_reconcile_an_existing_git_checkout_to_its_update_target_when_installing_without_a_ref()
 {
    let env = TestEnv::new();
    env.set_env("KIANA_FAKE_GIT_LOCAL_HEAD", "old-head");
    env.set_env("KIANA_FAKE_GIT_UPSTREAM_HEAD", "remote-head");
    env.set_env("KIANA_FAKE_GIT_TARGET_HEAD", "new-head");
    let target_dir = Path::new(&env.agent_dir).join("git/github.com/user/repo");
    std::fs::create_dir_all(&target_dir).expect("mkdir");
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);

    package_manager
        .install("git:github.com/user/repo", InstallOptions::default())
        .await
        .expect("install");

    let git_calls = env.calls("git");
    let command_lines: Vec<Vec<String>> = git_calls.iter().map(|call| call.args.clone()).collect();
    assert!(command_lines.contains(&vec![
        "fetch".to_string(),
        "--prune".to_string(),
        "--no-tags".to_string(),
        "origin".to_string(),
        "+refs/heads/main:refs/remotes/origin/main".to_string()
    ]));
    assert!(command_lines.contains(&vec![
        "reset".to_string(),
        "--hard".to_string(),
        "@{upstream}^{commit}".to_string()
    ]));
    assert!(command_lines.contains(&vec!["clean".to_string(), "-fdx".to_string()]));
}

#[tokio::test]
async fn should_detect_pnpm_through_a_corepack_wrapper_without_a_separator() {
    let env = TestEnv::new();
    env.set_env(
        "KIANA_FAKE_GIT_CLONE_PACKAGE_JSON",
        "{\"name\":\"repo\",\"version\":\"1.0.0\"}",
    );
    let settings_manager =
        env.settings_manager(settings_with_npm_command(vec!["corepack", "pnpm"]));
    let package_manager = env.manager(&settings_manager);
    package_manager
        .install("git:github.com/user/repo", InstallOptions::default())
        .await
        .expect("install");
    let target_dir = Path::new(&env.agent_dir).join("git/github.com/user/repo");
    let call = env.first_call("corepack").expect("corepack invocation");
    assert_eq!(
        call.args,
        vec![
            "pnpm".to_string(),
            "install".to_string(),
            "--prod".to_string(),
            "--config.auto-install-peers=false".to_string(),
            "--config.strict-peer-dependencies=false".to_string(),
            "--config.strict-dep-builds=false".to_string(),
        ]
    );
    assert_eq!(call.cwd, target_dir.to_string_lossy());
}

#[tokio::test]
async fn should_disable_peer_installation_for_git_package_dependencies_with_bun() {
    let env = TestEnv::new();
    env.set_env(
        "KIANA_FAKE_GIT_CLONE_PACKAGE_JSON",
        "{\"name\":\"repo\",\"version\":\"1.0.0\"}",
    );
    let settings_manager = env.settings_manager(settings_with_npm_command(vec!["bun"]));
    let package_manager = env.manager(&settings_manager);
    package_manager
        .install("git:github.com/user/repo", InstallOptions::default())
        .await
        .expect("install");
    let call = env.last_call("bun").expect("bun invocation");
    assert_eq!(
        call.args,
        vec![
            "install".to_string(),
            "--omit=dev".to_string(),
            "--omit=peer".to_string(),
        ]
    );
}

#[tokio::test]
async fn should_update_git_package_dependencies_without_auto_installing_peers() {
    let env = TestEnv::new();
    env.set_env("KIANA_FAKE_GIT_LOCAL_HEAD", "local-head");
    env.set_env("KIANA_FAKE_GIT_UPSTREAM_HEAD", "remote-head");
    env.set_env("KIANA_FAKE_GIT_TARGET_HEAD", "remote-head");
    let target_dir = Path::new(&env.root()).join(".kiana/git/github.com/user/repo");
    env.write(
        target_dir.join("package.json"),
        "{\"name\":\"repo\",\"version\":\"1.0.0\"}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager
        .set_project_packages(vec![text("git:github.com/user/repo")])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);

    package_manager
        .update(Some("git:github.com/user/repo"))
        .await
        .expect("update");

    let npm_call = env.last_call("npm").expect("npm invocation");
    assert_eq!(
        npm_call.args,
        vec![
            "install".to_string(),
            "--omit=dev".to_string(),
            "--legacy-peer-deps".to_string(),
        ]
    );
    assert_eq!(npm_call.cwd, target_dir.to_string_lossy());
}

#[tokio::test]
async fn should_repair_missing_git_package_dependencies_when_the_checkout_is_already_current() {
    let env = TestEnv::new();
    env.set_env("KIANA_FAKE_GIT_LOCAL_HEAD", "current-head");
    env.set_env("KIANA_FAKE_GIT_UPSTREAM_HEAD", "current-head");
    env.set_env("KIANA_FAKE_GIT_TARGET_HEAD", "current-head");
    let target_dir = Path::new(&env.agent_dir).join("git/github.com/user/repo");
    env.write(
        target_dir.join("package.json"),
        "{\"name\":\"repo\",\"version\":\"1.0.0\",\"dependencies\":{\"dependency\":\"1.0.0\"}}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_packages(vec![text("git:github.com/user/repo")]);
    let package_manager = env.manager(&settings_manager);

    package_manager
        .update(Some("git:github.com/user/repo"))
        .await
        .expect("update");

    let npm_call = env.last_call("npm").expect("npm invocation");
    assert_eq!(
        npm_call.args,
        vec![
            "install".to_string(),
            "--omit=dev".to_string(),
            "--legacy-peer-deps".to_string(),
        ]
    );
    assert_eq!(npm_call.cwd, target_dir.to_string_lossy());
    assert!(
        !env.calls("git")
            .iter()
            .any(|call| call.args == vec!["clean".to_string(), "-fdx".to_string()])
    );
}

#[tokio::test]
async fn should_repair_deleted_git_package_dependencies_when_cleaning_fails() {
    let env = TestEnv::new();
    env.set_env("KIANA_FAKE_GIT_LOCAL_HEAD", "old-head");
    env.set_env("KIANA_FAKE_GIT_UPSTREAM_HEAD", "new-head");
    env.set_env("KIANA_FAKE_GIT_TARGET_HEAD", "new-head");
    env.set_env("KIANA_FAKE_GIT_CLEAN_FAIL", "1");
    let target_dir = Path::new(&env.agent_dir).join("git/github.com/user/repo");
    env.write(
        target_dir.join("package.json"),
        "{\"name\":\"repo\",\"version\":\"1.0.0\",\"dependencies\":{\"dependency\":\"1.0.0\"}}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_packages(vec![text("git:github.com/user/repo")]);
    let package_manager = env.manager(&settings_manager);

    let error = package_manager
        .update(Some("git:github.com/user/repo"))
        .await
        .expect_err("update must fail");
    assert!(error.message().contains("git clean -fdx"));
    assert!(error.message().contains("failed with code 1"));
    let npm_call = env.last_call("npm").expect("npm invocation");
    assert_eq!(
        npm_call.args,
        vec![
            "install".to_string(),
            "--omit=dev".to_string(),
            "--legacy-peer-deps".to_string(),
        ]
    );
}

#[tokio::test]
async fn should_disable_peer_installation_through_wrapped_pnpm_when_updating_git_dependencies() {
    let env = TestEnv::new();
    env.set_env("KIANA_FAKE_GIT_LOCAL_HEAD", "local-head");
    env.set_env("KIANA_FAKE_GIT_UPSTREAM_HEAD", "remote-head");
    env.set_env("KIANA_FAKE_GIT_TARGET_HEAD", "remote-head");
    let target_dir = Path::new(&env.root()).join(".kiana/git/github.com/user/repo");
    env.write(
        target_dir.join("package.json"),
        "{\"name\":\"repo\",\"version\":\"1.0.0\"}",
    );
    let settings_manager = env.settings_manager(settings_with_npm_command(vec![
        "mise", "exec", "node@20", "--", "pnpm",
    ]));
    settings_manager
        .set_project_packages(vec![text("git:github.com/user/repo")])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);

    package_manager
        .update(Some("git:github.com/user/repo"))
        .await
        .expect("update");

    let call = env.first_call("mise").expect("mise invocation");
    assert_eq!(
        call.args,
        vec![
            "exec".to_string(),
            "node@20".to_string(),
            "--".to_string(),
            "pnpm".to_string(),
            "install".to_string(),
            "--prod".to_string(),
            "--config.auto-install-peers=false".to_string(),
            "--config.strict-peer-dependencies=false".to_string(),
            "--config.strict-dep-builds=false".to_string(),
        ]
    );
    assert_eq!(call.cwd, target_dir.to_string_lossy());
}

#[tokio::test]
async fn should_use_npm_command_argv_for_npm_root_lookup_and_invalidate_cached_root_when_npm_command_changes()
 {
    let env = TestEnv::new();
    let root20 = Path::new(&env.root()).join("node20/lib/node_modules");
    let root22 = Path::new(&env.root()).join("node22/lib/node_modules");
    std::fs::create_dir_all(root20.join("@scope/pkg")).expect("mkdir");
    let map_path = Path::new(&env.root()).join("mise-root-map.txt");
    std::fs::write(
        &map_path,
        format!(
            "node@20|{}\nnode@22|{}\n",
            root20.to_string_lossy(),
            root22.to_string_lossy()
        ),
    )
    .expect("write map");
    env.set_env("KIANA_FAKE_MISE_ROOT_MAP", &map_path.to_string_lossy());
    let settings_manager = env.settings_manager(settings_with_npm_command(vec![
        "mise", "exec", "node@20", "--", "npm",
    ]));
    let package_manager = env.manager(&settings_manager);

    assert_eq!(
        package_manager.get_installed_path("npm:@scope/pkg", InstallScope::User),
        Some(root20.join("@scope/pkg").to_string_lossy().into_owned())
    );
    let first = env.first_call("mise").expect("first lookup");
    assert_eq!(
        first.args,
        vec![
            "exec".to_string(),
            "node@20".to_string(),
            "--".to_string(),
            "npm".to_string(),
            "root".to_string(),
            "-g".to_string(),
        ]
    );

    settings_manager.set_npm_command(Some(vec![
        "mise".to_string(),
        "exec".to_string(),
        "node@22".to_string(),
        "--".to_string(),
        "npm".to_string(),
    ]));

    assert_eq!(
        package_manager.get_installed_path("npm:@scope/pkg", InstallScope::User),
        None
    );
    let calls = env.calls("mise");
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[1].args,
        vec![
            "exec".to_string(),
            "node@22".to_string(),
            "--".to_string(),
            "npm".to_string(),
            "root".to_string(),
            "-g".to_string(),
        ]
    );
}

#[tokio::test]
async fn should_install_user_npm_packages_into_the_kiana_managed_npm_root() {
    let env = TestEnv::new();
    env.set_env(
        "KIANA_FAKE_INSTALL_PACKAGE_JSON",
        "{\"name\":\"pnpm-pkg\",\"version\":\"1.0.0\"}",
    );
    env.set_env("KIANA_FAKE_INSTALL_FILE", "extensions/index.ts");
    let settings_manager = env.settings_manager(Settings {
        npm_command: Some(vec!["pnpm".to_string()]),
        packages: Some(vec![text("npm:pnpm-pkg")]),
        ..Settings::default()
    });
    let package_manager = env.manager(&settings_manager);

    let first = package_manager.resolve(None).await.expect("resolve");
    let second = package_manager.resolve(None).await.expect("resolve");

    let package_path = Path::new(&env.agent_dir).join("npm/node_modules/pnpm-pkg");
    assert!(first.extensions.iter().any(|resource| {
        resource.path == package_path.join("extensions/index.ts").to_string_lossy()
            && resource.enabled
    }));
    assert!(second.extensions.iter().any(|resource| {
        resource.path == package_path.join("extensions/index.ts").to_string_lossy()
            && resource.enabled
    }));
    let installs: Vec<_> = env
        .calls("pnpm")
        .into_iter()
        .filter(|call| call.args.first().map(String::as_str) == Some("install"))
        .collect();
    assert_eq!(installs.len(), 1);
    assert_eq!(
        package_manager.get_installed_path("npm:pnpm-pkg", InstallScope::User),
        Some(package_path.to_string_lossy().into_owned())
    );
    let npm_root = std::fs::canonicalize(Path::new(&env.agent_dir).join("npm"))
        .expect("realpath")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        installs[0].args,
        vec![
            "install".to_string(),
            "pnpm-pkg".to_string(),
            "--prefix".to_string(),
            npm_root,
            "--config.auto-install-peers=false".to_string(),
            "--config.strict-peer-dependencies=false".to_string(),
            "--config.strict-dep-builds=false".to_string(),
        ]
    );
}

#[tokio::test]
async fn should_load_legacy_pnpm_global_package_paths_from_pnpm_list_output() {
    let env = TestEnv::new();
    let pnpm_root = Path::new(&env.root()).join("pnpm/global/v11");
    let package_path = pnpm_root.join("20-hash/node_modules/pnpm-pkg");
    env.write(
        package_path.join("package.json"),
        "{\"name\":\"pnpm-pkg\",\"version\":\"1.0.0\"}",
    );
    env.write(
        package_path.join("extensions/index.ts"),
        "export default function() {};",
    );
    env.set_env(
        "KIANA_FAKE_PNPM_LIST",
        &format!(
            "[{{\"path\":\"{}\",\"dependencies\":{{\"pnpm-pkg\":{{\"version\":\"1.0.0\",\"path\":\"{}\"}}}}}}]",
            pnpm_root.to_string_lossy(),
            package_path.to_string_lossy()
        ),
    );
    let settings_manager = env.settings_manager(Settings {
        npm_command: Some(vec!["pnpm".to_string()]),
        packages: Some(vec![text("npm:pnpm-pkg")]),
        ..Settings::default()
    });
    let package_manager = env.manager(&settings_manager);

    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(result.extensions.iter().any(|resource| {
        resource.path == package_path.join("extensions/index.ts").to_string_lossy()
            && resource.enabled
    }));
    assert!(
        !env.calls("pnpm")
            .iter()
            .any(|call| call.args.first().map(String::as_str) == Some("install"))
    );
    assert_eq!(
        package_manager.get_installed_path("npm:pnpm-pkg", InstallScope::User),
        Some(package_path.to_string_lossy().into_owned())
    );
}

#[tokio::test]
async fn should_resolve_wrapped_pnpm_global_package_paths_from_pnpm_list_output() {
    let env = TestEnv::new();
    let pnpm_root = Path::new(&env.root()).join("pnpm/global/v11");
    let package_path = pnpm_root.join("20-hash/node_modules/pnpm-pkg");
    std::fs::create_dir_all(&package_path).expect("mkdir");
    env.set_env(
        "KIANA_FAKE_PNPM_LIST",
        &format!(
            "[{{\"path\":\"{}\",\"dependencies\":{{\"pnpm-pkg\":{{\"path\":\"{}\"}}}}}}]",
            pnpm_root.to_string_lossy(),
            package_path.to_string_lossy()
        ),
    );
    let settings_manager = env.settings_manager(settings_with_npm_command(vec![
        "mise", "exec", "node@20", "--", "pnpm",
    ]));
    let package_manager = env.manager(&settings_manager);

    assert_eq!(
        package_manager.get_installed_path("npm:pnpm-pkg", InstallScope::User),
        Some(package_path.to_string_lossy().into_owned())
    );
    let call = env.first_call("mise").expect("mise invocation");
    assert_eq!(
        call.args,
        vec![
            "exec".to_string(),
            "node@20".to_string(),
            "--".to_string(),
            "pnpm".to_string(),
            "list".to_string(),
            "-g".to_string(),
            "--depth".to_string(),
            "0".to_string(),
            "--json".to_string(),
        ]
    );
}

#[tokio::test]
async fn should_ignore_malformed_legacy_pnpm_global_package_lists() {
    let env = TestEnv::new();
    env.set_env("KIANA_FAKE_PNPM_LIST", "not json");
    let settings_manager = env.settings_manager(settings_with_npm_command(vec!["pnpm"]));
    let package_manager = env.manager(&settings_manager);
    assert_eq!(
        package_manager.get_installed_path("npm:pnpm-pkg", InstallScope::User),
        None
    );
}

// ===========================================================================
// describe("settings source normalization")
// ===========================================================================

#[tokio::test]
async fn should_store_global_local_packages_relative_to_agent_settings_base() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("packages/local-global-pkg");
    env.write(
        package_dir.join("extensions/index.ts"),
        "export default function() {}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);

    let added = package_manager
        .add_source_to_settings("./packages/local-global-pkg", InstallOptions::default());
    assert!(added);
    let settings = settings_manager.get_global_settings();
    assert_eq!(
        settings.packages.expect("packages"),
        vec![text("../packages/local-global-pkg")]
    );
}

#[tokio::test]
async fn should_store_project_local_packages_relative_to_kiana_settings_base() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("project-local-pkg");
    env.write(
        package_dir.join("extensions/index.ts"),
        "export default function() {}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);

    let added = package_manager
        .add_source_to_settings("./project-local-pkg", InstallOptions { local: true });
    assert!(added);
    let settings = settings_manager.get_project_settings();
    assert_eq!(
        settings.packages.expect("packages"),
        vec![text("../project-local-pkg")]
    );
}

#[tokio::test]
async fn should_remove_local_package_entries_using_equivalent_path_forms() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("remove-local-pkg");
    env.write(
        package_dir.join("extensions/index.ts"),
        "export default function() {}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);

    assert!(
        package_manager.add_source_to_settings("./remove-local-pkg", InstallOptions::default())
    );
    let removed = package_manager.remove_source_from_settings(
        &format!("{}/", package_dir.to_string_lossy()),
        InstallOptions::default(),
    );
    assert!(removed);
    assert!(
        settings_manager
            .get_global_settings()
            .packages
            .unwrap_or_default()
            .is_empty()
    );
}

#[tokio::test]
async fn should_return_false_when_adding_the_same_git_source_with_the_same_ref() {
    let env = TestEnv::new();
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    assert!(
        package_manager
            .add_source_to_settings("git:github.com/user/repo@v1", InstallOptions::default())
    );
    assert!(
        !package_manager
            .add_source_to_settings("git:github.com/user/repo@v1", InstallOptions::default())
    );
    assert_eq!(
        settings_manager
            .get_global_settings()
            .packages
            .expect("packages"),
        vec![text("git:github.com/user/repo@v1")]
    );
}

#[tokio::test]
async fn should_update_the_ref_when_adding_the_same_git_source_with_a_different_ref() {
    let env = TestEnv::new();
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    assert!(
        package_manager
            .add_source_to_settings("git:github.com/user/repo@v1", InstallOptions::default())
    );
    assert!(
        package_manager
            .add_source_to_settings("git:github.com/user/repo@v2", InstallOptions::default())
    );
    assert_eq!(
        settings_manager
            .get_global_settings()
            .packages
            .expect("packages"),
        vec![text("git:github.com/user/repo@v2")]
    );
}

#[tokio::test]
async fn should_preserve_package_filters_when_replacing_a_package_source_ref() {
    let env = TestEnv::new();
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_packages(vec![PackageSource::Object(PackageSourceObject {
        source: Some("git:github.com/user/repo@v1".to_string()),
        extensions: Some(vec!["extensions/main.ts".to_string()]),
        skills: Some(vec![]),
        prompts: Some(vec!["prompts/review.md".to_string()]),
        themes: Some(vec!["themes/dark.json".to_string()]),
        ..Default::default()
    })]);
    let package_manager = env.manager(&settings_manager);

    assert!(
        package_manager
            .add_source_to_settings("git:github.com/user/repo@v2", InstallOptions::default())
    );
    assert_eq!(
        settings_manager
            .get_global_settings()
            .packages
            .expect("packages"),
        vec![PackageSource::Object(PackageSourceObject {
            source: Some("git:github.com/user/repo@v2".to_string()),
            extensions: Some(vec!["extensions/main.ts".to_string()]),
            skills: Some(vec![]),
            prompts: Some(vec!["prompts/review.md".to_string()]),
            themes: Some(vec!["themes/dark.json".to_string()]),
            ..Default::default()
        })]
    );
}

// ===========================================================================
// describe("pattern filtering in top-level arrays")
// ===========================================================================

#[tokio::test]
async fn should_expand_tilde_in_exclusion_and_exact_patterns() {
    let env = TestEnv::new();
    let root = env.root();
    env.set_env("HOME", &root);
    let skills_dir = Path::new(&root).join("extra/skills");
    for name in ["keep", "glob-excluded", "exact-excluded"] {
        env.write(
            skills_dir.join(name).join("SKILL.md"),
            &format!("---\nname: {name}\ndescription: {name}\n---\n"),
        );
    }
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_skill_paths(vec![
        "~/extra/skills".to_string(),
        "!~/extra/skills/glob-*".to_string(),
        "-~/extra/skills/exact-excluded".to_string(),
    ]);
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(is_enabled(&result.skills, "keep/SKILL.md"));
    assert!(is_disabled(&result.skills, "glob-excluded/SKILL.md"));
    assert!(is_disabled(&result.skills, "exact-excluded/SKILL.md"));
}

#[tokio::test]
async fn should_exclude_extensions_with_bang_pattern() {
    let env = TestEnv::new();
    let extensions_dir = Path::new(&env.agent_dir).join("extensions");
    env.write(
        extensions_dir.join("keep.ts"),
        "export default function() {}",
    );
    env.write(
        extensions_dir.join("remove.ts"),
        "export default function() {}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager
        .set_extension_paths(vec!["extensions".to_string(), "!**/remove.ts".to_string()]);
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(is_enabled(&result.extensions, "keep.ts"));
    assert!(is_disabled(&result.extensions, "remove.ts"));
}

#[tokio::test]
async fn should_filter_themes_with_glob_patterns() {
    let env = TestEnv::new();
    let themes_dir = Path::new(&env.agent_dir).join("themes");
    for name in ["dark.json", "light.json", "funky.json"] {
        env.write(themes_dir.join(name), "{}");
    }
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_theme_paths(vec!["themes".to_string(), "!funky.json".to_string()]);
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(is_enabled(&result.themes, "dark.json"));
    assert!(is_enabled(&result.themes, "light.json"));
    assert!(is_disabled(&result.themes, "funky.json"));
}

#[tokio::test]
async fn should_filter_prompts_with_exclusion_pattern() {
    let env = TestEnv::new();
    let prompts_dir = Path::new(&env.agent_dir).join("prompts");
    env.write(prompts_dir.join("review.md"), "Review code");
    env.write(prompts_dir.join("explain.md"), "Explain code");
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager
        .set_prompt_template_paths(vec!["prompts".to_string(), "!explain.md".to_string()]);
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(is_enabled(&result.prompts, "review.md"));
    assert!(is_disabled(&result.prompts, "explain.md"));
}

#[tokio::test]
async fn should_filter_skills_with_exclusion_pattern() {
    let env = TestEnv::new();
    let skills_dir = Path::new(&env.agent_dir).join("skills");
    env.write(
        skills_dir.join("good-skill/SKILL.md"),
        "---\nname: good-skill\ndescription: Good\n---\nContent",
    );
    env.write(
        skills_dir.join("bad-skill/SKILL.md"),
        "---\nname: bad-skill\ndescription: Bad\n---\nContent",
    );
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_skill_paths(vec!["skills".to_string(), "!**/bad-skill".to_string()]);
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(is_enabled_includes(&result.skills, "good-skill"));
    assert!(is_disabled_includes(&result.skills, "bad-skill"));
}

#[tokio::test]
async fn should_work_without_patterns_backward_compatible() {
    let env = TestEnv::new();
    let extension_path = Path::new(&env.agent_dir).join("extensions/my-ext.ts");
    env.write(&extension_path, "export default function() {}");
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_extension_paths(vec!["extensions/my-ext.ts".to_string()]);
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(
        result
            .extensions
            .iter()
            .any(|resource| resource.path == extension_path.to_string_lossy() && resource.enabled)
    );
}

// ===========================================================================
// describe("pattern filtering in kiana manifest")
// ===========================================================================

#[tokio::test]
async fn should_support_glob_patterns_in_manifest_extensions() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("manifest-pkg");
    env.write(
        package_dir.join("extensions/local.ts"),
        "export default function() {}",
    );
    env.write(
        package_dir.join("node_modules/dep/extensions/remote.ts"),
        "export default function() {}",
    );
    env.write(
        package_dir.join("node_modules/dep/extensions/skip.ts"),
        "export default function() {}",
    );
    env.write(
        package_dir.join("package.json"),
        "{\"name\":\"manifest-pkg\",\"kiana\":{\"extensions\":[\"extensions\",\"node_modules/dep/extensions\",\"!**/skip.ts\"]}}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager
        .resolve_extension_sources(
            &[package_dir.to_string_lossy().into_owned()],
            ResolveExtensionSourcesOptions::default(),
        )
        .await
        .expect("resolve");
    assert!(is_enabled(&result.extensions, "local.ts"));
    assert!(is_enabled(&result.extensions, "remote.ts"));
    assert!(
        !result
            .extensions
            .iter()
            .any(|resource| resource.path.ends_with("skip.ts"))
    );
}

#[tokio::test]
async fn should_support_glob_patterns_in_manifest_skills() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("skill-manifest-pkg");
    env.write(
        package_dir.join("skills/good-skill/SKILL.md"),
        "---\nname: good-skill\ndescription: Good\n---\nContent",
    );
    env.write(
        package_dir.join("skills/bad-skill/SKILL.md"),
        "---\nname: bad-skill\ndescription: Bad\n---\nContent",
    );
    env.write(
        package_dir.join("package.json"),
        "{\"name\":\"skill-manifest-pkg\",\"kiana\":{\"skills\":[\"skills\",\"!**/bad-skill\"]}}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager
        .resolve_extension_sources(
            &[package_dir.to_string_lossy().into_owned()],
            ResolveExtensionSourcesOptions::default(),
        )
        .await
        .expect("resolve");
    assert!(is_enabled_includes(&result.skills, "good-skill"));
    assert!(
        !result
            .skills
            .iter()
            .any(|resource| resource.path.contains("bad-skill"))
    );
}

#[tokio::test]
async fn should_expand_positive_glob_manifest_entries_before_collecting_skills() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("skill-manifest-glob-pkg");
    env.write(
        package_dir.join("plugins/pdf-to-markdown/skills/pdf-to-markdown/SKILL.md"),
        "---\nname: pdf-to-markdown\ndescription: PDF to Markdown\n---\nContent",
    );
    env.write(
        package_dir.join("plugins/nutrient-dws/skills/document-processor-api/SKILL.md"),
        "---\nname: document-processor-api\ndescription: DWS\n---\nContent",
    );
    env.write(
        package_dir.join("package.json"),
        "{\"name\":\"skill-manifest-glob-pkg\",\"kiana\":{\"skills\":[\"./plugins/*/skills\"]}}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager
        .resolve_extension_sources(
            &[package_dir.to_string_lossy().into_owned()],
            ResolveExtensionSourcesOptions::default(),
        )
        .await
        .expect("resolve");
    assert!(is_enabled_includes(&result.skills, "pdf-to-markdown"));
    assert!(is_enabled_includes(
        &result.skills,
        "document-processor-api"
    ));
}

#[tokio::test]
async fn should_expand_trailing_globstar_manifest_entries_to_files() {
    // Node 的 fs.globSync("extensions/**") 返回目录本身与全部后代（含顶层文件）；
    // 旧实现只拿到子目录，顶层文件全部丢失。
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("trailing-globstar-pkg");
    for file in ["a.ts", "b.ts", "c.js", "keep.tmp", "sub/d.ts"] {
        env.write(
            package_dir.join("extensions").join(file),
            "export default function() {}",
        );
    }
    env.write(package_dir.join("skills/loose.md"), "Loose skill");
    env.write(
        package_dir.join("skills/deep/er/SKILL.md"),
        "---\nname: deep-skill\ndescription: Deep\n---\n",
    );
    env.write(
        package_dir.join("package.json"),
        "{\"name\":\"trailing-globstar-pkg\",\"kiana\":{\"extensions\":[\"extensions/**\"],\"skills\":[\"skills/**\"]}}",
    );

    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager
        .resolve_extension_sources(
            &[package_dir.to_string_lossy().into_owned()],
            ResolveExtensionSourcesOptions::default(),
        )
        .await
        .expect("resolve");

    let relative = |resources: &[kiana_resources::package::ResolvedResource]| -> Vec<String> {
        resources
            .iter()
            .map(|resource| {
                Path::new(&resource.path)
                    .strip_prefix(&package_dir)
                    .expect("under package")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    };
    assert_eq!(
        relative(&result.extensions),
        vec![
            "extensions/a.ts".to_string(),
            "extensions/b.ts".to_string(),
            "extensions/c.js".to_string(),
            "extensions/keep.tmp".to_string(),
            "extensions/sub/d.ts".to_string(),
        ]
    );
    // 顶层散装 markdown 与深层 SKILL.md 都要被收集（glob 直接命中的文件也算）。
    assert_eq!(
        relative(&result.skills),
        vec![
            "skills/deep/er/SKILL.md".to_string(),
            "skills/loose.md".to_string(),
        ]
    );
}

#[tokio::test]
async fn should_expand_brace_glob_manifest_entries() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("brace-glob-pkg");
    for file in ["a.ts", "b.ts", "c.ts"] {
        env.write(
            package_dir.join("ext").join(file),
            "export default function() {}",
        );
    }
    env.write(
        package_dir.join("other/c.ts"),
        "export default function() {}",
    );
    env.write(
        package_dir.join("other/d.js"),
        "export default function() {}",
    );
    env.write(
        package_dir.join("package.json"),
        "{\"name\":\"brace-glob-pkg\",\"kiana\":{\"extensions\":[\"./ext/{a,b}*.ts\",\"./other/*.{ts,js}\"]}}",
    );

    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager
        .resolve_extension_sources(
            &[package_dir.to_string_lossy().into_owned()],
            ResolveExtensionSourcesOptions::default(),
        )
        .await
        .expect("resolve");
    let relative: Vec<String> = result
        .extensions
        .iter()
        .map(|resource| {
            Path::new(&resource.path)
                .strip_prefix(&package_dir)
                .expect("under package")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        relative,
        vec![
            "ext/a.ts".to_string(),
            "ext/b.ts".to_string(),
            "other/c.ts".to_string(),
            "other/d.js".to_string(),
        ]
    );
}

#[tokio::test]
async fn should_sort_manifest_glob_matches_and_use_exact_entries_for_dot_paths_and_symlink_traversal()
 {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("manifest-glob-semantics-pkg");
    let extension_files_dir = package_dir.join("extension-files");
    let extension_group_dir = package_dir.join("extension-groups/group");
    let linked_plugin_source = package_dir.join("linked-plugin-source");
    env.write(
        extension_files_dir.join("z.ts"),
        "export default function() {}",
    );
    env.write(
        extension_files_dir.join("a.ts"),
        "export default function() {}",
    );
    env.write(
        extension_files_dir.join(".ignored.ts"),
        "export default function() {}",
    );
    env.write(
        extension_files_dir.join("nested/.hidden.ts"),
        "export default function() {}",
    );
    env.write(
        extension_group_dir.join("index.ts"),
        "export default function() {}",
    );
    env.write(
        package_dir.join("plugins/local/skills/local-skill/SKILL.md"),
        "---\nname: local-skill\ndescription: Local\n---\n",
    );
    env.write(
        linked_plugin_source.join("skills/linked-skill/SKILL.md"),
        "---\nname: linked-skill\ndescription: Linked\n---\n",
    );
    std::fs::create_dir_all(package_dir.join("plugins")).expect("mkdir");
    std::os::unix::fs::symlink(&linked_plugin_source, package_dir.join("plugins/linked"))
        .expect("symlink");
    env.write(
        package_dir.join("package.json"),
        "{\"name\":\"manifest-glob-semantics-pkg\",\"kiana\":{\"extensions\":[\"./extension-files/*.ts\",\"./extension-files/**/.ignored.ts\",\"./extension-files/nested/.hidden.ts\",\"./extension-groups/*/\"],\"skills\":[\"./plugins/*/skills\",\"./plugins/linked/skills\"]}}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager
        .resolve_extension_sources(
            &[package_dir.to_string_lossy().into_owned()],
            ResolveExtensionSourcesOptions::default(),
        )
        .await
        .expect("resolve");

    let relative_paths: Vec<String> = result
        .extensions
        .iter()
        .map(|resource| {
            std::path::Path::new(&resource.path)
                .strip_prefix(&package_dir)
                .expect("under package")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        relative_paths,
        vec![
            "extension-files/a.ts".to_string(),
            "extension-files/z.ts".to_string(),
            "extension-files/nested/.hidden.ts".to_string(),
            "extension-groups/group/index.ts".to_string(),
        ]
    );
    assert!(is_enabled_includes(&result.skills, "local-skill/SKILL.md"));
    assert!(is_enabled_includes(&result.skills, "linked-skill/SKILL.md"));
}

// ===========================================================================
// describe("pattern filtering in package filters")
// ===========================================================================

fn package_filter(
    source: &str,
    extensions: Option<Vec<&str>>,
    skills: Option<Vec<&str>>,
    prompts: Option<Vec<&str>>,
    themes: Option<Vec<&str>>,
) -> PackageSource {
    let map = |values: Option<Vec<&str>>| {
        values.map(|values| values.into_iter().map(str::to_string).collect())
    };
    PackageSource::Object(PackageSourceObject {
        source: Some(source.to_string()),
        extensions: map(extensions),
        skills: map(skills),
        prompts: map(prompts),
        themes: map(themes),
        ..Default::default()
    })
}

#[tokio::test]
async fn should_apply_user_filters_on_top_of_manifest_filters_not_replace() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("layered-pkg");
    env.write(
        package_dir.join("extensions/foo.ts"),
        "export default function() {}",
    );
    env.write(
        package_dir.join("extensions/bar.ts"),
        "export default function() {}",
    );
    env.write(
        package_dir.join("extensions/baz.ts"),
        "export default function() {}",
    );
    env.write(
        package_dir.join("package.json"),
        "{\"name\":\"layered-pkg\",\"kiana\":{\"extensions\":[\"extensions\",\"!**/baz.ts\"]}}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_packages(vec![package_filter(
        &package_dir.to_string_lossy(),
        Some(vec!["!**/bar.ts"]),
        Some(vec![]),
        Some(vec![]),
        Some(vec![]),
    )]);
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(is_enabled(&result.extensions, "foo.ts"));
    assert!(is_disabled(&result.extensions, "bar.ts"));
    assert!(
        !result
            .extensions
            .iter()
            .any(|resource| resource.path.ends_with("baz.ts"))
    );
}

#[tokio::test]
async fn should_exclude_extensions_from_package_with_bang_pattern() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("pattern-pkg");
    for name in ["foo.ts", "bar.ts", "baz.ts"] {
        env.write(
            package_dir.join("extensions").join(name),
            "export default function() {}",
        );
    }
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_packages(vec![package_filter(
        &package_dir.to_string_lossy(),
        Some(vec!["!**/baz.ts"]),
        Some(vec![]),
        Some(vec![]),
        Some(vec![]),
    )]);
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(is_enabled(&result.extensions, "foo.ts"));
    assert!(is_enabled(&result.extensions, "bar.ts"));
    assert!(is_disabled(&result.extensions, "baz.ts"));
}

#[tokio::test]
async fn should_filter_themes_from_package() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("theme-pkg");
    env.write(package_dir.join("themes/nice.json"), "{}");
    env.write(package_dir.join("themes/ugly.json"), "{}");
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_packages(vec![package_filter(
        &package_dir.to_string_lossy(),
        Some(vec![]),
        Some(vec![]),
        Some(vec![]),
        Some(vec!["!ugly.json"]),
    )]);
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(is_enabled(&result.themes, "nice.json"));
    assert!(is_disabled(&result.themes, "ugly.json"));
}

#[tokio::test]
async fn should_combine_include_and_exclude_patterns() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("combo-pkg");
    for name in ["alpha.ts", "beta.ts", "gamma.ts"] {
        env.write(
            package_dir.join("extensions").join(name),
            "export default function() {}",
        );
    }
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_packages(vec![package_filter(
        &package_dir.to_string_lossy(),
        Some(vec!["**/alpha.ts", "**/beta.ts", "!**/beta.ts"]),
        Some(vec![]),
        Some(vec![]),
        Some(vec![]),
    )]);
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(is_enabled(&result.extensions, "alpha.ts"));
    assert!(is_disabled(&result.extensions, "beta.ts"));
    assert!(is_disabled(&result.extensions, "gamma.ts"));
}

#[tokio::test]
async fn should_work_with_direct_paths_no_patterns() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("direct-pkg");
    env.write(
        package_dir.join("extensions/one.ts"),
        "export default function() {}",
    );
    env.write(
        package_dir.join("extensions/two.ts"),
        "export default function() {}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_packages(vec![package_filter(
        &package_dir.to_string_lossy(),
        Some(vec!["extensions/one.ts"]),
        Some(vec![]),
        Some(vec![]),
        Some(vec![]),
    )]);
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(is_enabled(&result.extensions, "one.ts"));
    assert!(is_disabled(&result.extensions, "two.ts"));
}

#[tokio::test]
async fn should_resolve_autoload_disabled_project_package_entries_as_deltas_over_global_packages() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.agent_dir).join("npm/node_modules/kiana-tools");
    env.write(
        package_dir.join("package.json"),
        "{\"name\":\"kiana-tools\",\"version\":\"1.0.0\"}",
    );
    env.write(
        package_dir.join("extensions/foo.ts"),
        "export default function() {}",
    );
    env.write(
        package_dir.join("extensions/bar.ts"),
        "export default function() {}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_packages(vec![text("npm:kiana-tools")]);
    settings_manager
        .set_project_packages(vec![PackageSource::Object(PackageSourceObject {
            source: Some("npm:kiana-tools".to_string()),
            autoload: Some(false),
            extensions: Some(vec!["-extensions/foo.ts".to_string()]),
            ..Default::default()
        })])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);

    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(env.calls("npm").is_empty(), "no install expected");
    let foo_path = package_dir.join("extensions/foo.ts");
    let bar_path = package_dir.join("extensions/bar.ts");
    let foo = result
        .extensions
        .iter()
        .find(|resource| resource.path == foo_path.to_string_lossy())
        .expect("foo");
    assert!(!foo.enabled);
    assert_eq!(
        foo.metadata.scope,
        kiana_core::source_info::SourceScope::Project
    );
    let bar = result
        .extensions
        .iter()
        .find(|resource| resource.path == bar_path.to_string_lossy())
        .expect("bar");
    assert!(bar.enabled);
    assert_eq!(
        bar.metadata.scope,
        kiana_core::source_info::SourceScope::User
    );
}

#[tokio::test]
async fn should_resolve_autoload_disabled_package_entries_as_positive_only_without_a_global_package()
 {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("positive-only-pkg");
    env.write(
        package_dir.join("extensions/foo.ts"),
        "export default function() {}",
    );
    env.write(
        package_dir.join("extensions/bar.ts"),
        "export default function() {}",
    );
    env.write(package_dir.join("skills/foo/SKILL.md"), "# Foo\n");
    // `relative(join(tempDir, ".kiana"), pkgDir)`：两者同在临时根下，包在 `.kiana siblings`。
    let relative_source = format!(
        "../{}",
        package_dir.file_name().expect("name").to_string_lossy()
    );
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager
        .set_project_packages(vec![PackageSource::Object(PackageSourceObject {
            source: Some(relative_source),
            autoload: Some(false),
            extensions: Some(vec!["+extensions/foo.ts".to_string()]),
            ..Default::default()
        })])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert_eq!(
        result
            .extensions
            .iter()
            .map(|resource| resource.path.clone())
            .collect::<Vec<_>>(),
        vec![
            package_dir
                .join("extensions/foo.ts")
                .to_string_lossy()
                .into_owned()
        ]
    );
    assert!(result.skills.is_empty());
}

// ===========================================================================
// describe("force-include patterns")
// ===========================================================================

#[tokio::test]
async fn should_force_include_extensions_with_plus_pattern_after_exclusion() {
    let env = TestEnv::new();
    let extensions_dir = Path::new(&env.agent_dir).join("extensions");
    for name in ["keep.ts", "excluded.ts", "force-back.ts"] {
        env.write(extensions_dir.join(name), "export default function() {}");
    }
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_extension_paths(vec![
        "extensions".to_string(),
        "!extensions/*.ts".to_string(),
        "+extensions/force-back.ts".to_string(),
    ]);
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(is_disabled(&result.extensions, "keep.ts"));
    assert!(is_disabled(&result.extensions, "excluded.ts"));
    assert!(is_enabled(&result.extensions, "force-back.ts"));
}

#[tokio::test]
async fn should_force_include_overrides_exclude_in_package_filters() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("force-pkg");
    for name in ["alpha.ts", "beta.ts", "gamma.ts"] {
        env.write(
            package_dir.join("extensions").join(name),
            "export default function() {}",
        );
    }
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_packages(vec![package_filter(
        &package_dir.to_string_lossy(),
        Some(vec!["!**/*.ts", "+extensions/beta.ts"]),
        Some(vec![]),
        Some(vec![]),
        Some(vec![]),
    )]);
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(is_disabled(&result.extensions, "alpha.ts"));
    assert!(is_enabled(&result.extensions, "beta.ts"));
    assert!(is_disabled(&result.extensions, "gamma.ts"));
}

#[tokio::test]
async fn should_force_include_multiple_resources() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("multi-force-pkg");
    for (name, description) in [("skill-a", "A"), ("skill-b", "B"), ("skill-c", "C")] {
        env.write(
            package_dir.join("skills").join(name).join("SKILL.md"),
            &format!("---\nname: {name}\ndescription: {description}\n---\nContent"),
        );
    }
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_packages(vec![package_filter(
        &package_dir.to_string_lossy(),
        Some(vec![]),
        Some(vec!["!**/*", "+skills/skill-a", "+skills/skill-c"]),
        Some(vec![]),
        Some(vec![]),
    )]);
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(is_enabled_includes(&result.skills, "skill-a"));
    assert!(is_disabled_includes(&result.skills, "skill-b"));
    assert!(is_enabled_includes(&result.skills, "skill-c"));
}

#[tokio::test]
async fn should_force_include_after_specific_exclusion() {
    let env = TestEnv::new();
    let extensions_dir = Path::new(&env.agent_dir).join("extensions");
    env.write(extensions_dir.join("a.ts"), "export default function() {}");
    env.write(extensions_dir.join("b.ts"), "export default function() {}");
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_extension_paths(vec![
        "extensions".to_string(),
        "!extensions/b.ts".to_string(),
        "+extensions/b.ts".to_string(),
    ]);
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(is_enabled(&result.extensions, "a.ts"));
    assert!(is_enabled(&result.extensions, "b.ts"));
}

#[tokio::test]
async fn should_handle_force_include_in_manifest_patterns() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("manifest-force-pkg");
    for name in ["one.ts", "two.ts", "three.ts"] {
        env.write(
            package_dir.join("extensions").join(name),
            "export default function() {}",
        );
    }
    env.write(
        package_dir.join("package.json"),
        "{\"name\":\"manifest-force-pkg\",\"kiana\":{\"extensions\":[\"extensions\",\"!**/two.ts\",\"+extensions/two.ts\"]}}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager
        .resolve_extension_sources(
            &[package_dir.to_string_lossy().into_owned()],
            ResolveExtensionSourcesOptions::default(),
        )
        .await
        .expect("resolve");
    assert!(is_enabled(&result.extensions, "one.ts"));
    assert!(is_enabled(&result.extensions, "two.ts"));
    assert!(is_enabled(&result.extensions, "three.ts"));
}

#[tokio::test]
async fn should_force_include_themes() {
    let env = TestEnv::new();
    let themes_dir = Path::new(&env.agent_dir).join("themes");
    for name in ["dark.json", "light.json", "special.json"] {
        env.write(themes_dir.join(name), "{}");
    }
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_theme_paths(vec![
        "themes".to_string(),
        "!themes/*.json".to_string(),
        "+themes/special.json".to_string(),
    ]);
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(is_disabled(&result.themes, "dark.json"));
    assert!(is_disabled(&result.themes, "light.json"));
    assert!(is_enabled(&result.themes, "special.json"));
}

#[tokio::test]
async fn should_force_include_prompts() {
    let env = TestEnv::new();
    let prompts_dir = Path::new(&env.agent_dir).join("prompts");
    for name in ["review.md", "explain.md", "debug.md"] {
        env.write(prompts_dir.join(name), "x");
    }
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_prompt_template_paths(vec![
        "prompts".to_string(),
        "!prompts/*.md".to_string(),
        "+prompts/debug.md".to_string(),
    ]);
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(is_disabled(&result.prompts, "review.md"));
    assert!(is_disabled(&result.prompts, "explain.md"));
    assert!(is_enabled(&result.prompts, "debug.md"));
}

// ===========================================================================
// describe("force-exclude patterns")
// ===========================================================================

#[tokio::test]
async fn should_force_exclude_top_level_resources() {
    let env = TestEnv::new();
    let extensions_dir = Path::new(&env.agent_dir).join("extensions");
    env.write(
        extensions_dir.join("alpha.ts"),
        "export default function() {}",
    );
    env.write(
        extensions_dir.join("beta.ts"),
        "export default function() {}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_extension_paths(vec![
        "extensions".to_string(),
        "+extensions/alpha.ts".to_string(),
        "-extensions/alpha.ts".to_string(),
    ]);
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(is_disabled(&result.extensions, "alpha.ts"));
    assert!(is_enabled(&result.extensions, "beta.ts"));
}

#[tokio::test]
async fn should_force_exclude_in_package_filters() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("force-exclude-pkg");
    env.write(
        package_dir.join("extensions/alpha.ts"),
        "export default function() {}",
    );
    env.write(
        package_dir.join("extensions/beta.ts"),
        "export default function() {}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_packages(vec![package_filter(
        &package_dir.to_string_lossy(),
        Some(vec![
            "extensions/*.ts",
            "+extensions/alpha.ts",
            "-extensions/alpha.ts",
        ]),
        Some(vec![]),
        Some(vec![]),
        Some(vec![]),
    )]);
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(is_disabled(&result.extensions, "alpha.ts"));
    assert!(is_enabled(&result.extensions, "beta.ts"));
}

// ===========================================================================
// describe("package deduplication")
// ===========================================================================

#[tokio::test]
async fn should_dedupe_same_local_package_in_global_and_project_project_wins() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("shared-pkg");
    env.write(
        package_dir.join("extensions/shared.ts"),
        "export default function() {}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_packages(vec![text(&package_dir.to_string_lossy())]);
    settings_manager
        .set_project_packages(vec![text(&package_dir.to_string_lossy())])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);

    let result = package_manager.resolve(None).await.expect("resolve");
    let shared: Vec<_> = result
        .extensions
        .iter()
        .filter(|resource| resource.path.contains("shared-pkg"))
        .collect();
    assert_eq!(shared.len(), 1);
    assert_eq!(
        shared[0].metadata.scope,
        kiana_core::source_info::SourceScope::Project
    );
}

#[tokio::test]
async fn should_keep_both_if_different_packages() {
    let env = TestEnv::new();
    let package1_dir = Path::new(&env.root()).join("pkg1");
    let package2_dir = Path::new(&env.root()).join("pkg2");
    env.write(
        package1_dir.join("extensions/from-pkg1.ts"),
        "export default function() {}",
    );
    env.write(
        package2_dir.join("extensions/from-pkg2.ts"),
        "export default function() {}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_packages(vec![text(&package1_dir.to_string_lossy())]);
    settings_manager
        .set_project_packages(vec![text(&package2_dir.to_string_lossy())])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(
        result
            .extensions
            .iter()
            .any(|resource| resource.path.contains("pkg1"))
    );
    assert!(
        result
            .extensions
            .iter()
            .any(|resource| resource.path.contains("pkg2"))
    );
}

// ===========================================================================
// describe("multi-file extension discovery (issue #1102)")
// ===========================================================================

#[tokio::test]
async fn should_only_load_index_ts_from_subdirectories_not_helper_modules() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("multifile-pkg");
    env.write(
        package_dir.join("extensions/subagent/index.ts"),
        "export default function(api) {}",
    );
    env.write(
        package_dir.join("extensions/subagent/agents.ts"),
        "export function helper() { return \"helper\"; }",
    );
    env.write(
        package_dir.join("extensions/standalone.ts"),
        "export default function(api) {}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager
        .resolve_extension_sources(
            &[package_dir.to_string_lossy().into_owned()],
            ResolveExtensionSourcesOptions::default(),
        )
        .await
        .expect("resolve");
    assert!(is_enabled(&result.extensions, "subagent/index.ts"));
    assert!(is_enabled(&result.extensions, "standalone.ts"));
    assert!(
        !result
            .extensions
            .iter()
            .any(|resource| resource.path.ends_with("agents.ts"))
    );
}

#[tokio::test]
async fn should_respect_package_json_kiana_extensions_manifest_in_subdirectories() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("manifest-subdir-pkg");
    env.write(
        package_dir.join("extensions/custom/package.json"),
        "{\"kiana\":{\"extensions\":[\"./main.ts\"]}}",
    );
    env.write(
        package_dir.join("extensions/custom/main.ts"),
        "export default function(api) {}",
    );
    env.write(
        package_dir.join("extensions/custom/utils.ts"),
        "export const util = 1;",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager
        .resolve_extension_sources(
            &[package_dir.to_string_lossy().into_owned()],
            ResolveExtensionSourcesOptions::default(),
        )
        .await
        .expect("resolve");
    assert!(is_enabled(&result.extensions, "custom/main.ts"));
    assert!(
        !result
            .extensions
            .iter()
            .any(|resource| resource.path.ends_with("utils.ts"))
    );
}

#[tokio::test]
async fn should_handle_mixed_top_level_files_and_subdirectories() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("mixed-pkg");
    env.write(
        package_dir.join("extensions/simple.ts"),
        "export default function(api) {}",
    );
    env.write(
        package_dir.join("extensions/complex/index.ts"),
        "export default function(api) {}",
    );
    env.write(
        package_dir.join("extensions/complex/a.ts"),
        "export const a = 1;",
    );
    env.write(
        package_dir.join("extensions/complex/b.ts"),
        "export const b = 2;",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager
        .resolve_extension_sources(
            &[package_dir.to_string_lossy().into_owned()],
            ResolveExtensionSourcesOptions::default(),
        )
        .await
        .expect("resolve");
    assert!(is_enabled(&result.extensions, "simple.ts"));
    assert!(is_enabled(&result.extensions, "complex/index.ts"));
    assert!(
        !result
            .extensions
            .iter()
            .any(|resource| resource.path.ends_with("complex/a.ts"))
    );
    assert!(
        !result
            .extensions
            .iter()
            .any(|resource| resource.path.ends_with("complex/b.ts"))
    );
    assert_eq!(
        result
            .extensions
            .iter()
            .filter(|resource| resource.enabled)
            .count(),
        2
    );
}

#[tokio::test]
async fn should_skip_subdirectories_without_index_ts_or_manifest() {
    let env = TestEnv::new();
    let package_dir = Path::new(&env.root()).join("no-entry-pkg");
    env.write(
        package_dir.join("extensions/broken/helper.ts"),
        "export const x = 1;",
    );
    env.write(
        package_dir.join("extensions/broken/another.ts"),
        "export const y = 2;",
    );
    env.write(
        package_dir.join("extensions/valid.ts"),
        "export default function(api) {}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager
        .resolve_extension_sources(
            &[package_dir.to_string_lossy().into_owned()],
            ResolveExtensionSourcesOptions::default(),
        )
        .await
        .expect("resolve");
    assert!(is_enabled(&result.extensions, "valid.ts"));
    assert_eq!(
        result
            .extensions
            .iter()
            .filter(|resource| resource.enabled)
            .count(),
        1
    );
}

// ===========================================================================
// describe("package install failures")
// ===========================================================================

#[tokio::test]
async fn should_skip_a_package_source_that_fails_to_install_and_report_it() {
    let env = TestEnv::new();
    env.set_env("KIANA_FAKE_FAIL_MATCH", "broken-package");
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager
        .set_project_packages(vec![text("npm:broken-package")])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);

    let result = package_manager.resolve(None).await.expect("resolve");
    let failures = result.install_failures.clone().expect("install failures");
    let install_root = Path::new(&env.root())
        .join(".kiana/npm")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        failures,
        vec![kiana_resources::package::PackageInstallFailure {
            source: "npm:broken-package".to_string(),
            scope: kiana_core::source_info::SourceScope::Project,
            message: format!(
                "npm install broken-package --prefix {install_root} --legacy-peer-deps failed with code 1"
            ),
        }]
    );
    assert!(result.extensions.is_empty());
}

#[tokio::test]
async fn should_not_report_install_failures_for_sources_that_are_skipped() {
    let env = TestEnv::new();
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager
        .set_project_packages(vec![text("npm:missing-package")])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);

    let result = package_manager
        .resolve(Some(kiana_resources::package::missing_source_callback(
            |_| async { kiana_resources::package::MissingSourceAction::Skip },
        )))
        .await
        .expect("resolve");
    assert!(result.install_failures.expect("failures").is_empty());
    assert!(env.calls("npm").is_empty());
}

#[tokio::test]
async fn should_keep_a_missing_source_an_error_when_the_caller_asks_for_one() {
    let env = TestEnv::new();
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager
        .set_project_packages(vec![text("npm:missing-package")])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);

    let error = package_manager
        .resolve(Some(kiana_resources::package::missing_source_callback(
            |_| async { kiana_resources::package::MissingSourceAction::Error },
        )))
        .await
        .expect_err("must fail");
    assert_eq!(error.message(), "Missing source: npm:missing-package");
}

// ===========================================================================
// describe("offline mode and network timeouts")
// ===========================================================================

#[tokio::test]
async fn should_update_npm_range_packages_using_the_configured_spec() {
    let env = TestEnv::new();
    let installed_path = Path::new(&env.root()).join(".kiana/npm/node_modules/example");
    env.write(
        installed_path.join("package.json"),
        "{\"name\":\"example\",\"version\":\"1.0.0\"}",
    );
    env.set_env("KIANA_FAKE_VIEW_OUTPUT", "[\"1.0.0\",\"1.2.0\"]");
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager
        .set_project_packages(vec![text("npm:example@^1.0.0")])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);

    package_manager
        .update(Some("npm:example"))
        .await
        .expect("update");

    let view = env.first_call("npm").expect("npm view");
    assert_eq!(
        view.args,
        vec![
            "view".to_string(),
            "example@^1.0.0".to_string(),
            "version".to_string(),
            "--json".to_string(),
        ]
    );
    assert_eq!(view.cwd, env.root());
    let install = env.last_call("npm").expect("npm install");
    let npm_root = std::fs::canonicalize(Path::new(&env.root()).join(".kiana/npm"))
        .expect("realpath")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        install.args,
        vec![
            "install".to_string(),
            "example@^1.0.0".to_string(),
            "--prefix".to_string(),
            npm_root,
            "--legacy-peer-deps".to_string(),
        ]
    );
}

#[tokio::test]
async fn should_skip_project_npm_update_when_installed_version_matches_latest() {
    let env = TestEnv::new();
    let installed_path = Path::new(&env.root()).join(".kiana/npm/node_modules/example");
    env.write(
        installed_path.join("package.json"),
        "{\"name\":\"example\",\"version\":\"1.3.1\"}",
    );
    env.set_env("KIANA_FAKE_VIEW_OUTPUT", "[\"1.0.0\",\"1.3.1\",\"1.0.2\"]");
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager
        .set_project_packages(vec![text("npm:example@^1.0.0")])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);

    package_manager
        .update(Some("npm:example"))
        .await
        .expect("update");
    assert!(
        !env.calls("npm")
            .iter()
            .any(|call| call.args.first().map(String::as_str) == Some("install"))
    );
}

#[tokio::test]
async fn should_skip_npm_updates_when_the_installed_version_is_newer_than_the_registry_version() {
    let env = TestEnv::new();
    let installed_path = Path::new(&env.root()).join(".kiana/npm/node_modules/example");
    env.write(
        installed_path.join("package.json"),
        "{\"name\":\"example\",\"version\":\"2.0.0\"}",
    );
    env.set_env("KIANA_FAKE_VIEW_OUTPUT", "\"1.9.0\"");
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager
        .set_project_packages(vec![text("npm:example")])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);

    package_manager
        .update(Some("npm:example"))
        .await
        .expect("update");
    assert!(
        !env.calls("npm")
            .iter()
            .any(|call| call.args.first().map(String::as_str) == Some("install"))
    );
}

#[tokio::test]
async fn should_migrate_legacy_user_npm_installs_into_the_managed_npm_root_during_update() {
    let env = TestEnv::new();
    let legacy_root = Path::new(&env.root()).join("legacy-global/node_modules");
    let legacy_path = legacy_root.join("legacy-pkg");
    env.write(
        legacy_path.join("package.json"),
        "{\"name\":\"legacy-pkg\",\"version\":\"1.0.0\"}",
    );
    env.set_env("KIANA_FAKE_NPM_ROOT", &legacy_root.to_string_lossy());
    env.set_env(
        "KIANA_FAKE_INSTALL_PACKAGE_JSON",
        "{\"name\":\"legacy-pkg\",\"version\":\"1.0.0\"}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_packages(vec![text("npm:legacy-pkg")]);
    let package_manager = env.manager(&settings_manager);

    assert_eq!(
        package_manager.get_installed_path("npm:legacy-pkg", InstallScope::User),
        Some(legacy_path.to_string_lossy().into_owned())
    );

    package_manager
        .update(Some("npm:legacy-pkg"))
        .await
        .expect("update");

    let managed_path = Path::new(&env.agent_dir).join("npm/node_modules/legacy-pkg");
    assert!(
        !env.calls("npm")
            .iter()
            .any(|call| call.args.first().map(String::as_str) == Some("view"))
    );
    let installs: Vec<_> = env
        .calls("npm")
        .into_iter()
        .filter(|call| call.args.first().map(String::as_str) == Some("install"))
        .collect();
    assert_eq!(installs.len(), 1);
    let npm_root = std::fs::canonicalize(Path::new(&env.agent_dir).join("npm"))
        .expect("realpath")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        installs[0].args,
        vec![
            "install".to_string(),
            "legacy-pkg@latest".to_string(),
            "--prefix".to_string(),
            npm_root,
            "--legacy-peer-deps".to_string(),
        ]
    );
    assert_eq!(
        package_manager.get_installed_path("npm:legacy-pkg", InstallScope::User),
        Some(managed_path.to_string_lossy().into_owned())
    );
}

#[tokio::test]
async fn should_batch_npm_updates_per_scope_and_run_git_updates_while_skipping_pinned_npm_and_current_packages()
 {
    let env = TestEnv::new();
    let user_old = Path::new(&env.agent_dir).join("npm/node_modules/user-old");
    let user_current = Path::new(&env.agent_dir).join("npm/node_modules/user-current");
    let user_unknown = Path::new(&env.agent_dir).join("npm/node_modules/user-unknown");
    let project_old = Path::new(&env.root()).join(".kiana/npm/node_modules/project-old");
    let project_current = Path::new(&env.root()).join(".kiana/npm/node_modules/project-current");
    for (path, name) in [
        (&user_old, "user-old"),
        (&user_current, "user-current"),
        (&user_unknown, "user-unknown"),
        (&project_old, "project-old"),
        (&project_current, "project-current"),
    ] {
        env.write(
            path.join("package.json"),
            &format!("{{\"name\":\"{name}\",\"version\":\"1.0.0\"}}"),
        );
    }
    let map_path = Path::new(&env.root()).join("view-map.txt");
    std::fs::write(
        &map_path,
        "user-old|\"2.0.0\"\nuser-current|\"1.0.0\"\nuser-unknown|FAIL\nproject-old|\"2.0.0\"\nproject-current|\"1.0.0\"\n",
    )
    .expect("write map");
    env.set_env("KIANA_FAKE_VIEW_MAP", &map_path.to_string_lossy());
    env.set_env("KIANA_FAKE_SLEEP_SECONDS", "0.01");
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_packages(vec![
        text("npm:user-old"),
        text("npm:user-current"),
        text("npm:user-unknown"),
        text("npm:user-pinned@1.0.0"),
        text("git:github.com/example/user-repo-a"),
        text("git:github.com/example/user-repo-b"),
        text("git:github.com/example/user-repo-pinned@v1"),
    ]);
    settings_manager
        .set_project_packages(vec![
            text("npm:project-old"),
            text("npm:project-current"),
            text("npm:project-missing"),
            text("git:github.com/example/project-repo-a"),
        ])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);

    package_manager.update(None).await.expect("update");

    let view_calls: Vec<_> = env
        .calls("npm")
        .into_iter()
        .filter(|call| call.args.first().map(String::as_str) == Some("view"))
        .collect();
    assert_eq!(view_calls.len(), 5);
    let installs: Vec<_> = env
        .calls("npm")
        .into_iter()
        .filter(|call| call.args.first().map(String::as_str) == Some("install"))
        .collect();
    assert_eq!(installs.len(), 2);
    let user_root = std::fs::canonicalize(Path::new(&env.agent_dir).join("npm"))
        .expect("realpath")
        .to_string_lossy()
        .into_owned();
    let project_root = std::fs::canonicalize(Path::new(&env.root()).join(".kiana/npm"))
        .expect("realpath")
        .to_string_lossy()
        .into_owned();
    // 两个批量更新在 `tokio::join!` 里并发，argv 的顺序不保证；按 prefix 匹配。
    let user_install = installs
        .iter()
        .find(|call| call.args.contains(&user_root))
        .expect("user batch");
    let project_install = installs
        .iter()
        .find(|call| call.args.contains(&project_root))
        .expect("project batch");
    assert_eq!(
        user_install.args,
        vec![
            "install".to_string(),
            "user-old@latest".to_string(),
            "user-unknown@latest".to_string(),
            "--prefix".to_string(),
            user_root,
            "--legacy-peer-deps".to_string(),
        ]
    );
    assert_eq!(
        project_install.args,
        vec![
            "install".to_string(),
            "project-old@latest".to_string(),
            "project-missing@latest".to_string(),
            "--prefix".to_string(),
            project_root,
            "--legacy-peer-deps".to_string(),
        ]
    );
    let clones: Vec<_> = env
        .calls("git")
        .into_iter()
        .filter(|call| call.args.first().map(String::as_str) == Some("clone"))
        .collect();
    assert_eq!(clones.len(), 4);
}

#[tokio::test]
async fn should_suggest_npm_source_prefixes_for_update_lookups() {
    let env = TestEnv::new();
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager
        .set_project_packages(vec![text("npm:example")])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);
    let error = package_manager
        .update(Some("example"))
        .await
        .expect_err("update must fail");
    assert_eq!(
        error.message(),
        "No matching package found for example. Did you mean npm:example?"
    );
}

#[tokio::test]
async fn should_suggest_git_source_prefixes_for_update_lookups() {
    let env = TestEnv::new();
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager
        .set_project_packages(vec![text("git:github.com/example/repo")])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);
    let error = package_manager
        .update(Some("github.com/example/repo"))
        .await
        .expect_err("update must fail");
    assert_eq!(
        error.message(),
        "No matching package found for github.com/example/repo. Did you mean git:github.com/example/repo?"
    );
}

#[tokio::test]
async fn should_skip_installing_missing_package_sources_when_offline() {
    let env = TestEnv::new();
    env.set_offline(true);
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager
        .set_project_packages(vec![
            text("npm:missing-package"),
            text("git:github.com/example/missing-repo"),
        ])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(
        !result
            .extensions
            .iter()
            .chain(result.skills.iter())
            .chain(result.prompts.iter())
            .chain(result.themes.iter())
            .any(|resource| resource.metadata.origin
                == kiana_core::source_info::SourceOrigin::Package)
    );
    assert!(env.calls("npm").is_empty());
    assert!(env.calls("git").is_empty());
}

/// `getTemporaryDir` 的独立复刻（测试需要预置缓存目录）。
fn temporary_git_dir(agent_dir: &str, host: &str, path: &str, r#ref: Option<&str>) -> String {
    let prefix = format!("git-{host}");
    let input = format!(
        "{prefix}-{path}{}",
        match r#ref {
            Some(value) => format!("@{value}"),
            None => String::new(),
        }
    );
    let hash = kiana_ai::model_catalog::sha256_hex(&input);
    Path::new(agent_dir)
        .join("tmp/extensions")
        .join(&prefix)
        .join(&hash[..8])
        .join(path)
        .to_string_lossy()
        .into_owned()
}

#[tokio::test]
async fn should_skip_refreshing_temporary_git_sources_when_offline() {
    let env = TestEnv::new();
    env.set_offline(true);
    let git_source = "git:github.com/example/repo";
    let installed_path = temporary_git_dir(&env.agent_dir, "github.com", "example/repo", None);
    env.write(
        Path::new(&installed_path).join("extensions/index.ts"),
        "export default function() {};",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager
        .resolve_extension_sources(
            &[git_source.to_string()],
            ResolveExtensionSourcesOptions {
                temporary: true,
                local: false,
            },
        )
        .await
        .expect("resolve");
    assert!(is_enabled(&result.extensions, "extensions/index.ts"));
    assert!(env.calls("git").is_empty());
}

#[tokio::test]
async fn should_load_a_new_checkout_when_a_pinned_temporary_git_source_changes_ref() {
    let env = TestEnv::new();
    let new_source = "git:github.com/example/repo@bbbbbbb";
    let old_path = temporary_git_dir(
        &env.agent_dir,
        "github.com",
        "example/repo",
        Some("aaaaaaa"),
    );
    env.write(
        Path::new(&old_path).join("extensions/old.ts"),
        "export default function() {};",
    );
    env.set_env("KIANA_FAKE_GIT_CLONE_FILE", "extensions/new.ts");
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let result = package_manager
        .resolve_extension_sources(
            &[new_source.to_string()],
            ResolveExtensionSourcesOptions {
                temporary: true,
                local: false,
            },
        )
        .await
        .expect("resolve");
    assert!(is_enabled(&result.extensions, "extensions/new.ts"));
    assert!(
        !result
            .extensions
            .iter()
            .any(|resource| resource.path.ends_with("extensions/old.ts"))
    );
    assert_eq!(env.calls("git").len(), 2, "clone + checkout");
}

#[tokio::test]
async fn should_not_run_npm_view_during_resolve_for_installed_unpinned_packages() {
    let env = TestEnv::new();
    env.set_offline(true);
    let installed_path = Path::new(&env.root()).join(".kiana/npm/node_modules/example");
    env.write(
        installed_path.join("package.json"),
        "{\"name\":\"example\",\"version\":\"1.0.0\"}",
    );
    env.write(
        installed_path.join("extensions/index.ts"),
        "export default function() {};",
    );
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager
        .set_project_packages(vec![text("npm:example@^1.0.0")])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);
    let result = package_manager.resolve(None).await.expect("resolve");
    assert!(is_enabled(&result.extensions, "extensions/index.ts"));
    assert!(env.calls("npm").is_empty());
}

#[tokio::test]
async fn should_reinstall_pinned_npm_packages_when_installed_version_does_not_match() {
    let env = TestEnv::new();
    let installed_path = Path::new(&env.root()).join(".kiana/npm/node_modules/example");
    env.write(
        installed_path.join("package.json"),
        "{\"name\":\"example\",\"version\":\"1.0.0\"}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager
        .set_project_packages(vec![text("npm:example@2.0.0")])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);
    package_manager.resolve(None).await.expect("resolve");
    let installs: Vec<_> = env
        .calls("npm")
        .into_iter()
        .filter(|call| call.args.first().map(String::as_str) == Some("install"))
        .collect();
    assert_eq!(installs.len(), 1);
}

#[tokio::test]
async fn should_not_check_package_updates_when_offline() {
    let env = TestEnv::new();
    env.set_offline(true);
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let updates = package_manager
        .check_for_available_updates()
        .await
        .expect("updates");
    assert!(updates.is_empty());
    assert!(env.calls("npm").is_empty());
}

#[tokio::test]
async fn should_report_updates_for_installed_unpinned_npm_packages() {
    let env = TestEnv::new();
    let installed_path = Path::new(&env.root()).join(".kiana/npm/node_modules/example");
    env.write(
        installed_path.join("package.json"),
        "{\"name\":\"example\",\"version\":\"1.0.0\"}",
    );
    env.set_env("KIANA_FAKE_VIEW_OUTPUT", "\"1.2.3\"");
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager
        .set_project_packages(vec![text("npm:example")])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);
    let updates = package_manager
        .check_for_available_updates()
        .await
        .expect("updates");
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].source, "npm:example");
    assert_eq!(updates[0].display_name, "example");
    assert_eq!(
        updates[0].package_type,
        kiana_resources::package::PackageType::Npm
    );
    assert_eq!(updates[0].scope, InstallScope::Project);
}

#[tokio::test]
async fn should_not_report_npm_updates_when_the_installed_version_is_newer_than_the_registry_version()
 {
    let env = TestEnv::new();
    let installed_path = Path::new(&env.root()).join(".kiana/npm/node_modules/example");
    env.write(
        installed_path.join("package.json"),
        "{\"name\":\"example\",\"version\":\"2.0.0\"}",
    );
    env.set_env("KIANA_FAKE_VIEW_OUTPUT", "\"1.9.0\"");
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager
        .set_project_packages(vec![text("npm:example")])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);
    let updates = package_manager
        .check_for_available_updates()
        .await
        .expect("updates");
    assert!(updates.is_empty());
}

#[tokio::test]
async fn should_skip_pinned_packages_when_checking_for_updates() {
    let env = TestEnv::new();
    let installed_npm = Path::new(&env.root()).join(".kiana/npm/node_modules/example");
    env.write(
        installed_npm.join("package.json"),
        "{\"name\":\"example\",\"version\":\"1.0.0\"}",
    );
    let installed_git = Path::new(&env.root()).join(".kiana/git/github.com/example/repo");
    std::fs::create_dir_all(&installed_git).expect("mkdir");
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager
        .set_project_packages(vec![
            text("npm:example@1.0.0"),
            text("git:github.com/example/repo@v1"),
        ])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);
    let updates = package_manager
        .check_for_available_updates()
        .await
        .expect("updates");
    assert!(updates.is_empty());
    assert!(env.calls("npm").is_empty());
    assert!(env.calls("git").is_empty());
}

#[tokio::test]
async fn should_use_npm_command_argv_for_npm_update_checks() {
    let env = TestEnv::new();
    let installed_path = Path::new(&env.root()).join(".kiana/npm/node_modules/example");
    env.write(
        installed_path.join("package.json"),
        "{\"name\":\"example\",\"version\":\"1.0.0\"}",
    );
    env.set_env("KIANA_FAKE_VIEW_OUTPUT", "\"1.2.3\"");
    let settings_manager = env.settings_manager(settings_with_npm_command(vec![
        "mise", "exec", "node@20", "--", "npm",
    ]));
    settings_manager
        .set_project_packages(vec![text("npm:example")])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);
    let updates = package_manager
        .check_for_available_updates()
        .await
        .expect("updates");
    assert_eq!(updates.len(), 1);
    let call = env.first_call("mise").expect("mise invocation");
    assert_eq!(
        call.args,
        vec![
            "exec".to_string(),
            "node@20".to_string(),
            "--".to_string(),
            "npm".to_string(),
            "view".to_string(),
            "example".to_string(),
            "version".to_string(),
            "--json".to_string(),
        ]
    );
    assert_eq!(call.cwd, env.root());
}

// ===========================================================================
// 其余公开 API（installAndPersist / removeAndPersist / listConfiguredPackages）
// ===========================================================================

#[tokio::test]
async fn should_install_and_persist_sources_in_settings() {
    let env = TestEnv::new();
    env.set_env(
        "KIANA_FAKE_INSTALL_PACKAGE_JSON",
        "{\"name\":\"persist-pkg\",\"version\":\"1.0.0\"}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    package_manager
        .install_and_persist("npm:persist-pkg", InstallOptions::default())
        .await
        .expect("install and persist");
    assert_eq!(
        settings_manager
            .get_global_settings()
            .packages
            .expect("packages"),
        vec![text("npm:persist-pkg")]
    );
    let installed = Path::new(&env.agent_dir).join("npm/node_modules/persist-pkg");
    assert_eq!(
        package_manager.get_installed_path("npm:persist-pkg", InstallScope::User),
        Some(installed.to_string_lossy().into_owned())
    );
}

#[tokio::test]
async fn should_remove_and_persist_sources_from_settings() {
    let env = TestEnv::new();
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_packages(vec![text("npm:removable")]);
    let package_manager = env.manager(&settings_manager);
    let removed = package_manager
        .remove_and_persist("npm:removable", InstallOptions::default())
        .await
        .expect("remove and persist");
    assert!(removed);
    assert!(
        settings_manager
            .get_global_settings()
            .packages
            .unwrap_or_default()
            .is_empty()
    );
}

#[tokio::test]
async fn should_list_configured_packages_in_user_then_project_order() {
    let env = TestEnv::new();
    let local_dir = Path::new(&env.root()).join("local-pkg");
    env.write(
        local_dir.join("extensions/index.ts"),
        "export default function() {}",
    );
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_packages(vec![
        text("npm:user-a"),
        PackageSource::Object(PackageSourceObject {
            source: Some("git:github.com/user/repo".to_string()),
            ..Default::default()
        }),
    ]);
    settings_manager
        .set_project_packages(vec![text(&local_dir.to_string_lossy())])
        .expect("set project");
    let package_manager = env.manager(&settings_manager);

    let configured = package_manager.list_configured_packages();
    assert_eq!(configured.len(), 3);
    assert_eq!(configured[0].source, "npm:user-a");
    assert_eq!(configured[0].scope, InstallScope::User);
    assert!(!configured[0].filtered);
    assert_eq!(configured[1].source, "git:github.com/user/repo");
    assert_eq!(configured[1].scope, InstallScope::User);
    assert!(configured[1].filtered);
    assert_eq!(configured[2].source, local_dir.to_string_lossy());
    assert_eq!(configured[2].scope, InstallScope::Project);
    assert!(!configured[2].filtered);
    assert_eq!(
        configured[2].installed_path,
        Some(local_dir.to_string_lossy().into_owned())
    );
}

// ===========================================================================
// describe("HTTPS git URL parsing (old behavior)")
// ===========================================================================

async fn install_and_clone(env: &TestEnv, source: &str) -> common::Invocation {
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    package_manager
        .install(source, InstallOptions::default())
        .await
        .expect("install");
    env.first_call("git").expect("git clone")
}

#[tokio::test]
async fn should_parse_https_github_urls_correctly() {
    let env = TestEnv::new();
    let clone = install_and_clone(&env, "https://github.com/user/repo").await;
    assert_eq!(clone.args[0], "clone");
    assert_eq!(clone.args[1], "https://github.com/user/repo");
    let target = Path::new(&env.agent_dir).join("git/github.com/user/repo");
    assert_eq!(clone.args[2], target.to_string_lossy());
    // 未固定 ref：不会执行 checkout。
    assert!(
        env.calls("git")
            .iter()
            .all(|call| call.args.first().map(String::as_str) != Some("checkout"))
    );
}

#[tokio::test]
async fn should_parse_https_urls_with_git_prefix() {
    let env = TestEnv::new();
    let clone = install_and_clone(&env, "git:https://github.com/user/repo").await;
    assert_eq!(clone.args[1], "https://github.com/user/repo");
    let target = Path::new(&env.agent_dir).join("git/github.com/user/repo");
    assert_eq!(clone.args[2], target.to_string_lossy());
}

#[tokio::test]
async fn should_parse_https_urls_with_ref() {
    let env = TestEnv::new();
    let clone = install_and_clone(&env, "https://github.com/user/repo@v1.2.3").await;
    assert_eq!(clone.args[1], "https://github.com/user/repo");
    let checkout = env
        .calls("git")
        .into_iter()
        .find(|call| call.args.first().map(String::as_str) == Some("checkout"))
        .expect("git checkout");
    assert_eq!(
        checkout.args,
        vec!["checkout".to_string(), "v1.2.3".to_string()]
    );
}

#[tokio::test]
async fn should_parse_host_path_shorthand_only_with_git_prefix() {
    let env = TestEnv::new();
    let clone = install_and_clone(&env, "git:github.com/user/repo").await;
    assert_eq!(clone.args[1], "https://github.com/user/repo");
    let target = Path::new(&env.agent_dir).join("git/github.com/user/repo");
    assert_eq!(clone.args[2], target.to_string_lossy());
}

#[tokio::test]
async fn should_treat_host_path_shorthand_as_local_without_git_prefix() {
    let env = TestEnv::new();
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let error = package_manager
        .install("github.com/user/repo", InstallOptions::default())
        .await
        .expect_err("local path does not exist");
    let resolved = Path::new(&env.root()).join("github.com/user/repo");
    assert_eq!(
        error.message(),
        format!("Path does not exist: {}", resolved.to_string_lossy())
    );
    assert!(env.calls("git").is_empty());
}

#[tokio::test]
async fn should_parse_https_urls_with_git_suffix() {
    let env = TestEnv::new();
    let clone = install_and_clone(&env, "https://github.com/user/repo.git").await;
    assert_eq!(clone.args[1], "https://github.com/user/repo.git");
    let target = Path::new(&env.agent_dir).join("git/github.com/user/repo");
    assert_eq!(clone.args[2], target.to_string_lossy());
}

#[tokio::test]
async fn should_parse_gitlab_https_urls() {
    let env = TestEnv::new();
    let clone = install_and_clone(&env, "https://gitlab.com/user/repo").await;
    assert_eq!(clone.args[1], "https://gitlab.com/user/repo");
    let target = Path::new(&env.agent_dir).join("git/gitlab.com/user/repo");
    assert_eq!(clone.args[2], target.to_string_lossy());
}

#[tokio::test]
async fn should_parse_bitbucket_https_urls() {
    let env = TestEnv::new();
    let clone = install_and_clone(&env, "https://bitbucket.org/user/repo").await;
    assert_eq!(clone.args[1], "https://bitbucket.org/user/repo");
    let target = Path::new(&env.agent_dir).join("git/bitbucket.org/user/repo");
    assert_eq!(clone.args[2], target.to_string_lossy());
}

#[tokio::test]
async fn should_parse_codeberg_https_urls() {
    let env = TestEnv::new();
    let clone = install_and_clone(&env, "https://codeberg.org/user/repo").await;
    assert_eq!(clone.args[1], "https://codeberg.org/user/repo");
    let target = Path::new(&env.agent_dir).join("git/codeberg.org/user/repo");
    assert_eq!(clone.args[2], target.to_string_lossy());
}

/// 变体 URL 列表（同一仓库的不同写法）。
const SAME_REPO_URLS: &[&str] = &[
    "https://github.com/user/repo",
    "https://github.com/user/repo.git",
    "ssh://git@github.com/user/repo",
    "git:https://github.com/user/repo",
    "git:github.com/user/repo",
    "git:git@github.com:user/repo",
    "git:git@github.com:user/repo.git",
];

async fn clone_targets_for(env: &TestEnv, packages: Vec<String>) -> Vec<String> {
    let settings_manager = env.settings_manager(Settings {
        packages: Some(packages.into_iter().map(|value| text(&value)).collect()),
        ..Settings::default()
    });
    let package_manager = env.manager(&settings_manager);
    package_manager.resolve(None).await.expect("resolve");
    env.calls("git")
        .into_iter()
        .filter(|call| call.args.first().map(String::as_str) == Some("clone"))
        .map(|call| call.args[2].clone())
        .collect()
}

#[tokio::test]
async fn should_generate_correct_package_identity_for_protocol_and_git_prefixed_urls() {
    let env = TestEnv::new();
    let targets = clone_targets_for(
        &env,
        [
            "https://github.com/user/repo",
            "https://github.com/user/repo@v1.0.0",
            "git:github.com/user/repo",
            "https://github.com/user/repo.git",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
    )
    .await;
    // 四种写法 identity 相同（忽略 ref 与 .git），只安装一次。
    assert_eq!(targets.len(), 1);
    let expected = Path::new(&env.agent_dir)
        .join("git/github.com/user/repo")
        .to_string_lossy()
        .into_owned();
    assert_eq!(targets[0], expected);
}

#[tokio::test]
async fn should_deduplicate_git_urls_with_different_supported_formats() {
    let env = TestEnv::new();
    let targets = clone_targets_for(
        &env,
        [
            "https://github.com/user/repo",
            "git:github.com/user/repo",
            "https://github.com/user/repo.git",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
    )
    .await;
    assert_eq!(targets.len(), 1);
}

#[tokio::test]
async fn should_handle_https_urls_with_refs_in_resolve() {
    let env = TestEnv::new();
    // `@main` 与 `@feature/branch` 都是固定 ref：安装时 checkout。
    let settings_manager = env.settings_manager(Settings {
        packages: Some(vec![
            text("https://github.com/user/repo@main"),
            text("git:github.com/user/other@feature/branch"),
        ]),
        ..Settings::default()
    });
    let package_manager = env.manager(&settings_manager);
    package_manager.resolve(None).await.expect("resolve");
    let checkouts: Vec<Vec<String>> = env
        .calls("git")
        .into_iter()
        .filter(|call| call.args.first().map(String::as_str) == Some("checkout"))
        .map(|call| call.args)
        .collect();
    assert!(checkouts.contains(&vec!["checkout".to_string(), "main".to_string()]));
    assert!(checkouts.contains(&vec!["checkout".to_string(), "feature/branch".to_string()]));
}

// ===========================================================================
// describe("package deduplication")（SSH/HTTPS 变体）
// ===========================================================================

#[tokio::test]
async fn should_dedupe_ssh_and_https_urls_for_same_repo() {
    let env = TestEnv::new();
    let targets = clone_targets_for(
        &env,
        vec![
            "https://github.com/user/repo".to_string(),
            "git:git@github.com:user/repo".to_string(),
        ],
    )
    .await;
    assert_eq!(targets.len(), 1);
    assert_eq!(
        targets[0],
        Path::new(&env.agent_dir)
            .join("git/github.com/user/repo")
            .to_string_lossy()
    );
}

#[tokio::test]
async fn should_dedupe_ssh_and_https_with_refs() {
    let env = TestEnv::new();
    let targets = clone_targets_for(
        &env,
        vec![
            "https://github.com/user/repo@v1.0.0".to_string(),
            "git:git@github.com:user/repo@v1.0.0".to_string(),
        ],
    )
    .await;
    assert_eq!(targets.len(), 1);
}

#[tokio::test]
async fn should_dedupe_ssh_url_with_ssh_protocol_and_git_at_format() {
    let env = TestEnv::new();
    let targets = clone_targets_for(
        &env,
        vec![
            "ssh://git@github.com/user/repo".to_string(),
            "git:git@github.com:user/repo".to_string(),
        ],
    )
    .await;
    assert_eq!(targets.len(), 1);
}

#[tokio::test]
async fn should_dedupe_all_supported_url_formats_for_same_repo() {
    let env = TestEnv::new();
    let targets = clone_targets_for(
        &env,
        SAME_REPO_URLS.iter().map(|url| url.to_string()).collect(),
    )
    .await;
    assert_eq!(targets.len(), 1);
    assert_eq!(
        targets[0],
        Path::new(&env.agent_dir)
            .join("git/github.com/user/repo")
            .to_string_lossy()
    );
}

#[tokio::test]
async fn should_keep_different_repos_separate_https_vs_ssh() {
    let env = TestEnv::new();
    let targets = clone_targets_for(
        &env,
        vec![
            "https://github.com/user/repo1".to_string(),
            "git:git@github.com:user/repo2".to_string(),
        ],
    )
    .await;
    assert_eq!(targets.len(), 2);
    let first = Path::new(&env.agent_dir)
        .join("git/github.com/user/repo1")
        .to_string_lossy()
        .into_owned();
    let second = Path::new(&env.agent_dir)
        .join("git/github.com/user/repo2")
        .to_string_lossy()
        .into_owned();
    assert!(targets.contains(&first));
    assert!(targets.contains(&second));
}

// ===========================================================================
// describe("npmCommand") / describe("version checks")
// ===========================================================================

#[tokio::test]
async fn should_prefer_the_package_manager_after_a_separator_over_the_outer_executable() {
    let env = TestEnv::new();
    env.set_env(
        "KIANA_FAKE_GIT_CLONE_PACKAGE_JSON",
        "{\"name\":\"git-pkg\"}",
    );
    let settings_manager = env.settings_manager(Settings {
        npm_command: Some(vec![
            "npm".to_string(),
            "exec".to_string(),
            "--".to_string(),
            "pnpm".to_string(),
        ]),
        ..Settings::default()
    });
    let package_manager = env.manager(&settings_manager);
    package_manager
        .install("git:github.com/user/repo", InstallOptions::default())
        .await
        .expect("install");

    let npm = env.calls("npm");
    assert_eq!(npm.len(), 1, "expected one dependency install");
    assert_eq!(
        npm[0].args,
        vec![
            "exec",
            "--",
            "pnpm",
            "install",
            "--prod",
            "--config.auto-install-peers=false",
            "--config.strict-peer-dependencies=false",
            "--config.strict-dep-builds=false",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn should_use_npm_view_to_fetch_latest_version() {
    let env = TestEnv::new();
    env.set_env("KIANA_FAKE_VIEW_OUTPUT", "\"1.2.3\"");
    let installed = Path::new(&env.agent_dir).join("npm/node_modules/example");
    env.write(
        installed.join("package.json"),
        "{\"name\":\"example\",\"version\":\"1.0.0\"}",
    );
    let settings_manager = env.settings_manager(Settings {
        packages: Some(vec![text("npm:example")]),
        ..Settings::default()
    });
    let package_manager = env.manager(&settings_manager);

    let updates = package_manager
        .check_for_available_updates()
        .await
        .expect("updates");
    assert_eq!(
        updates,
        vec![kiana_resources::package::PackageUpdate {
            source: "npm:example".to_string(),
            display_name: "example".to_string(),
            package_type: kiana_resources::package::PackageType::Npm,
            scope: InstallScope::User,
        }]
    );
    let npm = env.calls("npm");
    assert_eq!(npm.len(), 1);
    assert_eq!(
        npm[0].args,
        vec!["view", "example", "version", "--json"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
    );
    assert_eq!(npm[0].cwd, env.root());
}

// ===========================================================================
// project trust（冻结签名下的 Result 变体）
// ===========================================================================

#[test]
fn should_expose_trust_errors_through_result_helpers() {
    let env = TestEnv::new();
    let settings_manager = kiana_config::settings::SettingsManager::in_memory_with_options(
        Settings::default(),
        kiana_config::settings::SettingsManagerCreateOptions {
            project_trusted: Some(false),
        },
    );
    let package_manager = env.manager(&settings_manager);

    let error = package_manager
        .try_get_installed_path("npm:example", InstallScope::Project)
        .expect_err("untrusted project");
    assert_eq!(
        error.message(),
        "Project is not trusted; refusing to access project package storage"
    );
    assert_eq!(
        package_manager.get_installed_path("npm:example", InstallScope::Project),
        None
    );

    // 本地源需要项目基准目录 → `getBaseDirForScope` 先抛「拒绝访问」。
    let error = package_manager
        .try_add_source_to_settings("./local-pkg", InstallOptions { local: true })
        .expect_err("untrusted project");
    assert_eq!(
        error.message(),
        "Project is not trusted; refusing to access project package storage"
    );
    assert!(!package_manager.add_source_to_settings("./local-pkg", InstallOptions { local: true }));

    // 非本地源走到 setProjectPackages → 「拒绝写入」。
    let error = package_manager
        .try_add_source_to_settings("npm:example", InstallOptions { local: true })
        .expect_err("untrusted project");
    assert_eq!(
        error.message(),
        "Project is not trusted; refusing to write project settings"
    );

    // 空的项目配置过滤后没有变化：TS 也不会触达 project 存储，返回 false。
    assert!(
        !package_manager
            .try_remove_source_from_settings("./local-pkg", InstallOptions { local: true })
            .expect("no matching entry")
    );
    assert!(
        !package_manager.remove_source_from_settings("./local-pkg", InstallOptions { local: true })
    );
}

#[tokio::test]
async fn should_reject_package_objects_without_a_source() {
    let env = TestEnv::new();
    // TS: `pkg.source` 是 undefined，`parseSource(undefined)` 抛 TypeError；绝不能把
    // settings 基准目录当成包加载。
    env.write(
        Path::new(&env.root()).join(".kiana/extensions/bogus.ts"),
        "export default function() {}",
    );
    let mut settings = Settings::default();
    settings
        .extra
        .insert("packages".to_string(), serde_json::json!([{"skills": []}]));
    let settings_manager = env.settings_manager(settings);
    let package_manager = env.manager(&settings_manager);
    let error = package_manager
        .resolve(None)
        .await
        .expect_err("missing source must fail");
    assert_eq!(
        error.message(),
        "Cannot read properties of undefined (reading 'startsWith')"
    );
}
