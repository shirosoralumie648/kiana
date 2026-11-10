//! 移植 `packages/coding-agent/test/resource-loader.test.ts`（57 个测试）。
//!
//! 测试标题逐字保留。少数断言依赖 `ExtensionRunner` 的命令重名解析（`deploy:1` /
//! `deploy:2`）：本 crate 只负责加载，命令解析归 `kiana-extensions::runner`，因此这里断言
//! 「两个扩展都保留、各自注册的命令内容正确」这一等价事实（见 deviations）。

#[path = "resource_loader_support/mod.rs"]
mod support;

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use kiana_config::settings::SettingsManager;
use kiana_config::settings::schema::{PackageSource, Settings};
use kiana_ext_api::types::{InlineExtension, LoadExtensionsResult};
use kiana_resources::loader::ResourceLoaderReloadOptions;
use kiana_resources::loader::context_files::load_project_context_files;

use support::{
    EMPTY_EXTENSION, LoaderHarness, TestDirs, command_extension, command_factory, new_loader,
    recording_factory, tool_extension, write_file,
};

fn s(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// 串行化改动 `KIANA_OFFLINE` 的测试（同一测试二进制内的线程共享进程环境）。
static OFFLINE_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 临时清除 `KIANA_OFFLINE`（TS 的 `vi.stubEnv("KIANA_OFFLINE", "")`），drop 时恢复原值。
struct OfflineEnvGuard {
    previous: Option<std::ffi::OsString>,
}

impl OfflineEnvGuard {
    fn clear() -> Self {
        let previous = std::env::var_os("KIANA_OFFLINE");
        unsafe {
            std::env::remove_var("KIANA_OFFLINE");
        }
        Self { previous }
    }
}

impl Drop for OfflineEnvGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => unsafe {
                std::env::set_var("KIANA_OFFLINE", value);
            },
            None => unsafe {
                std::env::remove_var("KIANA_OFFLINE");
            },
        }
    }
}

fn dark_theme_with_name(name: &str) -> String {
    let mut theme: serde_json::Value =
        serde_json::from_str(kiana_theme::DARK_JSON).expect("parse dark theme");
    theme["name"] = serde_json::Value::String(name.to_string());
    serde_json::to_string(&theme).expect("serialize theme")
}

const USER_TRUST_EXTENSION: &str = r#"
const token = Math.random().toString(36);
export default function(kiana) {
    kiana.registerCommand("user-trust", {
        description: token,
        handler: async () => {},
    });
}
"#;

// ---------------------------------------------------------------------------
// describe("DefaultResourceLoader") > describe("reload")
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn should_initialize_with_empty_results_before_reload() {
    let dirs = TestDirs::new();
    let loader = new_loader(&dirs, |_| {});

    assert!(loader.get_extensions().extensions.is_empty());
    assert!(loader.get_skills().skills.is_empty());
    assert!(loader.get_prompts().prompts.is_empty());
    assert!(loader.get_themes().themes.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn should_not_treat_a_project_manifest_as_the_owner_of_a_project_extension() {
    let _harness = LoaderHarness::new();
    let dirs = TestDirs::new();
    write_file(
        &dirs.cwd,
        "package.json",
        r#"{"dependencies":{"@earendil-works/kiana-coding-agent":"1.0.0"}}"#,
    );
    write_file(
        &dirs.cwd,
        ".kiana/extensions/project-extension.ts",
        EMPTY_EXTENSION,
    );

    let loader = new_loader(&dirs, |_| {});
    loader.reload(None).await.expect("reload");

    let result = loader.get_extensions();
    assert_eq!(result.extensions.len(), 1);
    assert_eq!(result.warnings.unwrap_or_default(), Vec::new());
}

#[tokio::test(flavor = "current_thread")]
async fn should_warn_about_host_dependencies_in_an_extension_package_manifest() {
    let _harness = LoaderHarness::new();
    let dirs = TestDirs::new();
    let package_root = dirs.temp_path("extension-package");
    write_file(
        &package_root,
        "package.json",
        r#"{"dependencies":{"@earendil-works/kiana-coding-agent":"1.0.0"}}"#,
    );
    write_file(
        &package_root,
        "extensions/package-extension.ts",
        EMPTY_EXTENSION,
    );

    let settings = SettingsManager::in_memory(Settings {
        packages: Some(vec![PackageSource::Text(s(&package_root))]),
        ..Default::default()
    });
    let loader = new_loader(&dirs, |options| {
        options.settings_manager = Some(settings);
    });
    loader.reload(None).await.expect("reload");

    let result = loader.get_extensions();
    assert_eq!(result.extensions.len(), 1);
    assert_eq!(
        result.warnings.unwrap_or_default(),
        vec![kiana_ext_api::types::LoadWarning {
            path: s(&package_root.join("package.json")),
            warning: "Host-provided extension packages must be declared in peerDependencies with a \"*\" range, not dependencies: @earendil-works/kiana-coding-agent. Installed copies can bypass the extension loader and create duplicate runtime modules.".to_string(),
        }]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn should_warn_and_keep_loading_when_a_configured_package_cannot_be_installed() {
    let _env_lock = OFFLINE_ENV_LOCK.lock().await;
    let _offline = OfflineEnvGuard::clear();
    let dirs = TestDirs::new();
    let settings = SettingsManager::in_memory(Settings {
        packages: Some(vec![PackageSource::Text("npm:broken-package".to_string())]),
        npm_command: Some(vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "exit 1".to_string(),
        ]),
        ..Default::default()
    });
    let loader = new_loader(&dirs, |options| {
        options.settings_manager = Some(settings);
    });

    loader.reload(None).await.expect("reload");

    let warnings = loader.get_extensions().warnings.unwrap_or_default();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].path, "npm:broken-package");
    assert!(
        warnings[0].warning.starts_with("failed to install ("),
        "{}",
        warnings[0].warning
    );
    assert!(warnings[0].warning.contains("failed with code 1)."));
    assert!(
        warnings[0]
            .warning
            .contains("Run \"kiana remove npm:broken-package\" to remove it from settings.json.")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn should_point_temporary_extension_sources_at_the_source_instead_of_kiana_remove() {
    let _env_lock = OFFLINE_ENV_LOCK.lock().await;
    let _offline = OfflineEnvGuard::clear();
    let dirs = TestDirs::new();
    let settings = SettingsManager::in_memory(Settings {
        npm_command: Some(vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "exit 1".to_string(),
        ]),
        ..Default::default()
    });
    let loader = new_loader(&dirs, |options| {
        options.settings_manager = Some(settings);
        options.additional_extension_paths = vec!["npm:broken-package".to_string()];
    });

    loader.reload(None).await.expect("reload");

    let warnings = loader.get_extensions().warnings.unwrap_or_default();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].path, "npm:broken-package");
    assert!(
        warnings[0]
            .warning
            .contains("Check the source and try again.")
    );
    assert!(!warnings[0].warning.contains("kiana remove"));
}

#[tokio::test(flavor = "current_thread")]
async fn should_point_project_extension_packages_at_kiana_remove_local() {
    let _env_lock = OFFLINE_ENV_LOCK.lock().await;
    let _offline = OfflineEnvGuard::clear();
    let dirs = TestDirs::new();
    write_file(
        &dirs.agent_dir,
        "settings.json",
        r#"{"npmCommand":["/bin/sh","-c","exit 1"]}"#,
    );
    write_file(
        &dirs.cwd,
        ".kiana/settings.json",
        r#"{"packages":["npm:broken-package"]}"#,
    );
    let settings = SettingsManager::create_with_options(
        &s(&dirs.cwd),
        &s(&dirs.agent_dir),
        kiana_config::settings::SettingsManagerCreateOptions {
            project_trusted: Some(true),
        },
    );
    let loader = new_loader(&dirs, |options| {
        options.settings_manager = Some(settings);
    });

    loader.reload(None).await.expect("reload");

    let warnings = loader.get_extensions().warnings.unwrap_or_default();
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0]
            .warning
            .contains("Run \"kiana remove npm:broken-package --local\""),
        "{}",
        warnings[0].warning
    );
}

#[tokio::test(flavor = "current_thread")]
async fn should_warn_once_about_a_failing_install_when_project_trust_is_resolved() {
    let _env_lock = OFFLINE_ENV_LOCK.lock().await;
    let _offline = OfflineEnvGuard::clear();
    let dirs = TestDirs::new();
    let settings = SettingsManager::in_memory(Settings {
        packages: Some(vec![PackageSource::Text("npm:broken-package".to_string())]),
        npm_command: Some(vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "exit 1".to_string(),
        ]),
        ..Default::default()
    });
    let loader = new_loader(&dirs, |options| {
        options.settings_manager = Some(settings);
    });

    let reload_options = ResourceLoaderReloadOptions {
        resolve_project_trust: Some(Box::new(|_| Box::pin(async { false }))),
    };
    loader.reload(Some(reload_options)).await.expect("reload");

    let warnings = loader.get_extensions().warnings.unwrap_or_default();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].path, "npm:broken-package");
}

#[tokio::test(flavor = "current_thread")]
async fn should_fail_when_an_extension_package_manifest_cannot_be_parsed() {
    let dirs = TestDirs::new();
    let package_root = dirs.temp_path("invalid-extension-package");
    write_file(&package_root, "package.json", "{");
    write_file(
        &package_root,
        "extensions/package-extension.ts",
        EMPTY_EXTENSION,
    );

    let settings = SettingsManager::in_memory(Settings {
        packages: Some(vec![PackageSource::Text(s(&package_root))]),
        ..Default::default()
    });
    let loader = new_loader(&dirs, |options| {
        options.settings_manager = Some(settings);
    });

    let error = loader.reload(None).await.expect_err("reload must fail");
    assert!(matches!(
        error,
        kiana_resources::loader::ResourceLoaderError::Manifest(_)
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn should_fail_when_an_extension_package_manifest_is_null() {
    // TS：`JSON.parse("null").dependencies` 抛 TypeError（未捕获，reload() reject）。
    let dirs = TestDirs::new();
    let package_root = dirs.temp_path("null-extension-package");
    write_file(&package_root, "package.json", "null");
    write_file(
        &package_root,
        "extensions/package-extension.ts",
        EMPTY_EXTENSION,
    );

    let settings = SettingsManager::in_memory(Settings {
        packages: Some(vec![PackageSource::Text(s(&package_root))]),
        ..Default::default()
    });
    let loader = new_loader(&dirs, |options| {
        options.settings_manager = Some(settings);
    });

    let error = loader.reload(None).await.expect_err("reload must fail");
    assert!(matches!(
        error,
        kiana_resources::loader::ResourceLoaderError::Manifest(_)
    ));
    assert_eq!(
        error.to_string(),
        "Cannot read properties of null (reading 'dependencies')"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn should_discover_skills_from_agent_dir() {
    let dirs = TestDirs::new();
    write_file(
        &dirs.agent_dir,
        "skills/test-skill.md",
        "---\nname: test-skill\ndescription: A test skill\n---\nSkill content here.",
    );

    let loader = new_loader(&dirs, |_| {});
    loader.reload(None).await.expect("reload");

    let skills = loader.get_skills().skills;
    assert!(skills.iter().any(|skill| skill.name == "test-skill"));
}

#[tokio::test(flavor = "current_thread")]
async fn should_ignore_extra_markdown_files_in_auto_discovered_skill_dirs() {
    let dirs = TestDirs::new();
    write_file(
        &dirs.agent_dir,
        "skills/kiana-skills/browser-tools/SKILL.md",
        "---\nname: browser-tools\ndescription: Browser tools\n---\nSkill content here.",
    );
    write_file(
        &dirs.agent_dir,
        "skills/kiana-skills/browser-tools/EFFICIENCY.md",
        "No frontmatter here",
    );

    let loader = new_loader(&dirs, |_| {});
    loader.reload(None).await.expect("reload");

    let result = loader.get_skills();
    assert!(
        result
            .skills
            .iter()
            .any(|skill| skill.name == "browser-tools")
    );
    assert!(!result.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .path
            .as_deref()
            .is_some_and(|path| path.ends_with("EFFICIENCY.md"))
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn should_discover_prompts_from_agent_dir() {
    let dirs = TestDirs::new();
    write_file(
        &dirs.agent_dir,
        "prompts/test-prompt.md",
        "---\ndescription: A test prompt\n---\nPrompt content.",
    );

    let loader = new_loader(&dirs, |_| {});
    loader.reload(None).await.expect("reload");

    assert!(
        loader
            .get_prompts()
            .prompts
            .iter()
            .any(|prompt| prompt.name == "test-prompt")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn should_report_invalid_prompt_frontmatter_while_loading_valid_siblings() {
    let dirs = TestDirs::new();
    let invalid_prompt_path = dirs.agent_dir.join("prompts/invalid.md");
    write_file(
        &dirs.agent_dir,
        "prompts/invalid.md",
        "---\ndescription: Broken: unquoted colon\n---\nDo something.\n",
    );
    write_file(&dirs.agent_dir, "prompts/valid.md", "Valid prompt content.");

    let loader = new_loader(&dirs, |_| {});
    loader.reload(None).await.expect("reload");

    let result = loader.get_prompts();
    assert_eq!(
        result
            .prompts
            .iter()
            .map(|prompt| prompt.name.clone())
            .collect::<Vec<_>>(),
        vec!["valid".to_string()]
    );
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(
        result.diagnostics[0].kind,
        kiana_core::diagnostics::DiagnosticKind::Warning
    );
    assert_eq!(
        result.diagnostics[0].path.as_deref(),
        Some(s(&invalid_prompt_path).as_str())
    );
    assert!(result.diagnostics[0].message.contains("line 1, column 14"));
}

#[tokio::test(flavor = "current_thread")]
async fn should_prefer_project_resources_over_user_on_name_collisions() {
    let dirs = TestDirs::new();
    let user_prompt_path = write_file(&dirs.agent_dir, "prompts/commit.md", "User prompt");
    let project_prompt_path = write_file(&dirs.cwd, ".kiana/prompts/commit.md", "Project prompt");
    let user_skill_path = write_file(
        &dirs.agent_dir,
        "skills/collision-skill/SKILL.md",
        "---\nname: collision-skill\ndescription: user\n---\nUser skill",
    );
    let project_skill_path = write_file(
        &dirs.cwd,
        ".kiana/skills/collision-skill/SKILL.md",
        "---\nname: collision-skill\ndescription: project\n---\nProject skill",
    );
    let user_theme_path = write_file(
        &dirs.agent_dir,
        "themes/collision.json",
        &dark_theme_with_name("collision-theme"),
    );
    let project_theme_path = write_file(
        &dirs.cwd,
        ".kiana/themes/collision.json",
        &dark_theme_with_name("collision-theme"),
    );

    let loader = new_loader(&dirs, |_| {});
    loader.reload(None).await.expect("reload");

    let prompt = loader
        .get_prompts()
        .prompts
        .into_iter()
        .find(|prompt| prompt.name == "commit")
        .expect("commit prompt");
    assert_eq!(prompt.file_path, s(&project_prompt_path));
    let _ = user_prompt_path;

    let skill = loader
        .get_skills()
        .skills
        .into_iter()
        .find(|skill| skill.name == "collision-skill")
        .expect("collision skill");
    assert_eq!(skill.file_path, s(&project_skill_path));
    let _ = user_skill_path;

    let theme = loader
        .get_themes()
        .themes
        .into_iter()
        .find(|theme| theme.name() == Some("collision-theme"))
        .expect("collision theme");
    assert_eq!(theme.source_path().map(s), Some(s(&project_theme_path)));
    let _ = user_theme_path;
}

#[tokio::test(flavor = "current_thread")]
async fn should_load_symlinked_user_and_project_extensions_once() {
    let _harness = LoaderHarness::new();
    let dirs = TestDirs::new();
    let shared_ext_dir = dirs.temp_path("shared-extensions");
    write_file(
        &shared_ext_dir,
        "shared.ts",
        &command_extension("shared", "shared command"),
    );
    std::fs::create_dir_all(&dirs.agent_dir).expect("agent dir");
    std::fs::create_dir_all(dirs.cwd.join(".kiana")).expect("project .kiana");
    std::os::unix::fs::symlink(&shared_ext_dir, dirs.agent_dir.join("extensions"))
        .expect("symlink agent extensions");
    std::os::unix::fs::symlink(&shared_ext_dir, dirs.cwd.join(".kiana/extensions"))
        .expect("symlink project extensions");

    let loader = new_loader(&dirs, |_| {});
    loader.reload(None).await.expect("reload");

    let result = loader.get_extensions();
    assert_eq!(result.extensions.len(), 1);
    assert_eq!(result.errors, Vec::new());
    // mergePaths 先处理项目路径，因此项目别名是规范化后的幸存者。
    assert_eq!(
        result.extensions[0].path,
        s(&dirs.cwd.join(".kiana/extensions/shared.ts"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn should_load_user_extensions_before_trust_and_reuse_them_after_trust_resolves() {
    let _harness = LoaderHarness::new();
    let dirs = TestDirs::new();
    let user_ext_path = write_file(&dirs.agent_dir, "extensions/user.ts", USER_TRUST_EXTENSION);
    let project_ext_path = write_file(
        &dirs.cwd,
        ".kiana/extensions/project.ts",
        &command_extension("project-trusted", "project trusted"),
    );

    let loader = new_loader(&dirs, |_| {});
    let pre_trust_paths: Rc<RefCell<Option<Vec<String>>>> = Rc::new(RefCell::new(None));
    let pre_trust_token: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let paths_in = Rc::clone(&pre_trust_paths);
    let token_in = Rc::clone(&pre_trust_token);
    let reload_options = ResourceLoaderReloadOptions {
        resolve_project_trust: Some(Box::new(move |result: LoadExtensionsResult| {
            *paths_in.borrow_mut() = Some(
                result
                    .extensions
                    .iter()
                    .map(|extension| extension.path.clone())
                    .collect(),
            );
            *token_in.borrow_mut() = result
                .extensions
                .iter()
                .find_map(|extension| extension.commands.get("user-trust"))
                .and_then(|command| command.description.clone());
            Box::pin(async { true })
        })),
    };
    loader.reload(Some(reload_options)).await.expect("reload");

    let result = loader.get_extensions();
    assert_eq!(
        result
            .extensions
            .iter()
            .map(|extension| extension.path.clone())
            .collect::<Vec<_>>(),
        vec![s(&project_ext_path), s(&user_ext_path)]
    );
    assert_eq!(
        pre_trust_paths.borrow().clone(),
        Some(vec![s(&user_ext_path)])
    );
    // 模块体只执行一次（工厂缓存）：预信任与最终两次加载得到同一个 module 级 token。
    let final_token = result
        .extensions
        .iter()
        .find_map(|extension| extension.commands.get("user-trust"))
        .and_then(|command| command.description.clone());
    assert_eq!(pre_trust_token.borrow().clone(), final_token);
}

#[tokio::test(flavor = "current_thread")]
async fn should_keep_both_extensions_loaded_when_command_names_collide() {
    let _harness = LoaderHarness::new();
    let dirs = TestDirs::new();
    let project_ext_path = write_file(
        &dirs.cwd,
        ".kiana/extensions/project.ts",
        "export default function(kiana) {\n  kiana.registerCommand(\"deploy\", { description: \"project deploy\", handler: async () => {} });\n  kiana.registerCommand(\"project-only\", { description: \"project only\", handler: async () => {} });\n}\n",
    );
    let user_ext_path = write_file(
        &dirs.agent_dir,
        "extensions/user.ts",
        "export default function(kiana) {\n  kiana.registerCommand(\"deploy\", { description: \"user deploy\", handler: async () => {} });\n  kiana.registerCommand(\"user-only\", { description: \"user only\", handler: async () => {} });\n}\n",
    );

    let loader = new_loader(&dirs, |_| {});
    loader.reload(None).await.expect("reload");

    let result = loader.get_extensions();
    assert_eq!(result.extensions.len(), 2);
    assert!(
        !result
            .errors
            .iter()
            .any(|error| error.error.contains("Command \"/deploy\" conflicts"))
    );
    let project = result
        .extensions
        .iter()
        .find(|extension| extension.path == s(&project_ext_path))
        .expect("project extension");
    let user = result
        .extensions
        .iter()
        .find(|extension| extension.path == s(&user_ext_path))
        .expect("user extension");
    assert_eq!(
        project.commands["deploy"].description.as_deref(),
        Some("project deploy")
    );
    assert_eq!(
        user.commands["deploy"].description.as_deref(),
        Some("user deploy")
    );
    assert!(project.commands.contains_key("project-only"));
    assert!(user.commands.contains_key("user-only"));
}

#[tokio::test(flavor = "current_thread")]
async fn should_honor_overrides_for_auto_discovered_resources() {
    let _harness = LoaderHarness::new();
    let dirs = TestDirs::new();
    let settings_manager = SettingsManager::in_memory(Settings::default());
    settings_manager.set_extension_paths(vec!["-extensions/disabled.ts".to_string()]);
    settings_manager.set_skill_paths(vec!["-skills/skip-skill".to_string()]);
    settings_manager.set_prompt_template_paths(vec!["-prompts/skip.md".to_string()]);
    settings_manager.set_theme_paths(vec!["-themes/skip.json".to_string()]);

    write_file(&dirs.agent_dir, "extensions/disabled.ts", EMPTY_EXTENSION);
    write_file(
        &dirs.agent_dir,
        "skills/skip-skill/SKILL.md",
        "---\nname: skip-skill\ndescription: Skip me\n---\nContent",
    );
    write_file(&dirs.agent_dir, "prompts/skip.md", "Skip prompt");
    write_file(&dirs.agent_dir, "themes/skip.json", "{}");

    let loader = new_loader(&dirs, |options| {
        options.settings_manager = Some(settings_manager);
    });
    loader.reload(None).await.expect("reload");

    let extensions = loader.get_extensions();
    assert!(
        !extensions
            .extensions
            .iter()
            .any(|extension| extension.path.ends_with("disabled.ts"))
    );
    assert_eq!(extensions.errors, Vec::new());
    assert!(
        !loader
            .get_skills()
            .skills
            .iter()
            .any(|skill| skill.name == "skip-skill")
    );
    assert!(
        !loader
            .get_prompts()
            .prompts
            .iter()
            .any(|prompt| prompt.name == "skip")
    );
    assert!(!loader.get_themes().themes.iter().any(|theme| {
        theme
            .source_path()
            .is_some_and(|path| path.ends_with("skip.json"))
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn should_discover_agents_md_context_files() {
    let dirs = TestDirs::new();
    write_file(
        &dirs.cwd,
        "AGENTS.md",
        "# Project Guidelines\n\nBe helpful.",
    );

    let loader = new_loader(&dirs, |_| {});
    loader.reload(None).await.expect("reload");

    assert!(
        loader
            .get_agents_files()
            .agents_files
            .iter()
            .any(|file| file.path.contains("AGENTS.md"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn should_prefer_agents_override_md_within_each_directory_while_preserving_ancestor_layering()
{
    let dirs = TestDirs::new();
    let nested_cwd = dirs.cwd.join("service");
    std::fs::create_dir_all(&nested_cwd).expect("nested cwd");
    write_file(&dirs.agent_dir, "AGENTS.md", "global instructions");
    write_file(&dirs.agent_dir, "AGENTS.override.md", "global override");
    write_file(&dirs.cwd, "AGENTS.md", "project instructions");
    write_file(&nested_cwd, "AGENTS.md", "service instructions");
    write_file(&nested_cwd, "AGENTS.override.md", "service override");

    let loader = new_loader(&dirs, |options| {
        options.cwd = s(&nested_cwd);
    });
    loader.reload(None).await.expect("reload");

    let files: Vec<(String, String)> = loader
        .get_agents_files()
        .agents_files
        .into_iter()
        .map(|file| (file.path, file.content))
        .collect();
    assert_eq!(
        files,
        vec![
            (
                s(&dirs.agent_dir.join("AGENTS.override.md")),
                "global override".to_string()
            ),
            (
                s(&dirs.cwd.join("AGENTS.md")),
                "project instructions".to_string()
            ),
            (
                s(&nested_cwd.join("AGENTS.override.md")),
                "service override".to_string()
            ),
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn should_ignore_context_file_candidates_that_are_directories() {
    let dirs = TestDirs::new();
    std::fs::create_dir_all(dirs.cwd.join("AGENTS.override.md")).expect("dir override");
    std::fs::create_dir_all(dirs.cwd.join("AGENTS.md")).expect("dir agents");
    let claude_path = write_file(&dirs.cwd, "CLAUDE.md", "Fallback instructions");

    let loader = new_loader(&dirs, |_| {});
    loader.reload(None).await.expect("reload");

    assert!(
        loader
            .get_agents_files()
            .agents_files
            .iter()
            .any(|file| file.path == s(&claude_path) && file.content == "Fallback instructions")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn should_skip_context_file_discovery_when_no_context_files_is_true() {
    let dirs = TestDirs::new();
    write_file(
        &dirs.cwd,
        "AGENTS.override.md",
        "# Override Guidelines\n\nBe helpful.",
    );
    write_file(
        &dirs.cwd,
        "AGENTS.md",
        "# Project Guidelines\n\nBe helpful.",
    );
    write_file(&dirs.cwd, "CLAUDE.md", "# Claude Guidelines\n\nBe helpful.");

    let loader = new_loader(&dirs, |options| {
        options.no_context_files = true;
    });
    loader.reload(None).await.expect("reload");

    assert_eq!(loader.get_agents_files().agents_files, Vec::new());
}

#[tokio::test(flavor = "current_thread")]
async fn should_discover_system_md_from_cwd_kiana() {
    let dirs = TestDirs::new();
    write_file(
        &dirs.cwd,
        ".kiana/SYSTEM.md",
        "You are a helpful assistant.",
    );

    let loader = new_loader(&dirs, |_| {});
    loader.reload(None).await.expect("reload");

    assert_eq!(
        loader.get_system_prompt().as_deref(),
        Some("You are a helpful assistant.")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn should_skip_project_resources_that_require_trust_when_project_is_not_trusted() {
    let dirs = TestDirs::new();
    write_file(&dirs.cwd, ".kiana/SYSTEM.md", "Project system prompt.");
    write_file(&dirs.agent_dir, "SYSTEM.md", "Global system prompt.");
    write_file(&dirs.agent_dir, "AGENTS.md", "Global instructions");
    write_file(&dirs.cwd, "AGENTS.md", "Project instructions");
    write_file(
        &dirs.cwd,
        ".kiana/extensions/project.ts",
        "throw new Error(\"should not load\");",
    );
    write_file(
        &dirs.cwd,
        ".kiana/skills/project-skill/SKILL.md",
        "---\nname: project-skill\ndescription: Project skill\n---\nProject skill content",
    );
    write_file(&dirs.cwd, ".kiana/prompts/project.md", "Project prompt");
    write_file(
        &dirs.cwd,
        ".kiana/themes/project.json",
        &dark_theme_with_name("project-theme"),
    );
    let settings = SettingsManager::create_with_options(
        &s(&dirs.cwd),
        &s(&dirs.agent_dir),
        kiana_config::settings::SettingsManagerCreateOptions {
            project_trusted: Some(false),
        },
    );

    let loader = new_loader(&dirs, |options| {
        options.settings_manager = Some(settings);
    });
    loader.reload(None).await.expect("reload");

    assert_eq!(
        loader.get_system_prompt().as_deref(),
        Some("Global system prompt.")
    );
    let agents_files = loader.get_agents_files().agents_files;
    assert!(
        agents_files
            .iter()
            .any(|file| file.path == s(&dirs.agent_dir.join("AGENTS.md")))
    );
    assert!(
        agents_files
            .iter()
            .any(|file| file.path == s(&dirs.cwd.join("AGENTS.md")))
    );
    let extensions = loader.get_extensions();
    assert_eq!(extensions.extensions.len(), 0);
    assert_eq!(extensions.errors, Vec::new());
    assert!(
        !loader
            .get_skills()
            .skills
            .iter()
            .any(|skill| skill.name == "project-skill")
    );
    assert!(
        !loader
            .get_prompts()
            .prompts
            .iter()
            .any(|prompt| prompt.name == "project")
    );
    assert!(
        !loader
            .get_themes()
            .themes
            .iter()
            .any(|theme| theme.name() == Some("project-theme"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn should_discover_append_system_md() {
    let dirs = TestDirs::new();
    write_file(
        &dirs.cwd,
        ".kiana/APPEND_SYSTEM.md",
        "Additional instructions.",
    );

    let loader = new_loader(&dirs, |_| {});
    loader.reload(None).await.expect("reload");

    assert!(
        loader
            .get_append_system_prompt()
            .contains(&"Additional instructions.".to_string())
    );
}

// ---------------------------------------------------------------------------
// describe("system prompt sources")
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn exposes_discovered_project_system_md_as_the_system_prompt_source() {
    let dirs = TestDirs::new();
    let system_prompt_path = write_file(&dirs.cwd, ".kiana/SYSTEM.md", "Project system prompt.");

    let loader = new_loader(&dirs, |_| {});
    loader.reload(None).await.expect("reload");

    assert_eq!(
        loader.get_system_prompt().as_deref(),
        Some("Project system prompt.")
    );
    assert_eq!(
        loader.get_system_prompt_source(),
        Some(kiana_resources::loader::SystemPromptSource {
            path: s(&system_prompt_path)
        })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn exposes_discovered_global_system_md_as_the_system_prompt_source() {
    let dirs = TestDirs::new();
    let system_prompt_path = write_file(&dirs.agent_dir, "SYSTEM.md", "Global system prompt.");

    let loader = new_loader(&dirs, |_| {});
    loader.reload(None).await.expect("reload");

    assert_eq!(
        loader.get_system_prompt().as_deref(),
        Some("Global system prompt.")
    );
    assert_eq!(
        loader.get_system_prompt_source(),
        Some(kiana_resources::loader::SystemPromptSource {
            path: s(&system_prompt_path)
        })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn does_not_expose_literal_system_prompt_text_as_a_source() {
    let dirs = TestDirs::new();
    let loader = new_loader(&dirs, |options| {
        options.system_prompt = Some("Literal system prompt.".to_string());
    });
    loader.reload(None).await.expect("reload");

    assert_eq!(
        loader.get_system_prompt().as_deref(),
        Some("Literal system prompt.")
    );
    assert_eq!(loader.get_system_prompt_source(), None);
}

#[tokio::test(flavor = "current_thread")]
async fn exposes_file_backed_system_prompt_options_as_a_source() {
    let dirs = TestDirs::new();
    let system_prompt_path = write_file(
        dirs.temp.path(),
        "custom-system.md",
        "Custom system prompt.",
    );
    let loader = new_loader(&dirs, |options| {
        options.system_prompt = Some(s(&system_prompt_path));
    });
    loader.reload(None).await.expect("reload");

    assert_eq!(
        loader.get_system_prompt().as_deref(),
        Some("Custom system prompt.")
    );
    assert_eq!(
        loader.get_system_prompt_source(),
        Some(kiana_resources::loader::SystemPromptSource {
            path: s(&system_prompt_path)
        })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn exposes_discovered_append_system_md_as_an_append_system_prompt_source() {
    let dirs = TestDirs::new();
    let append_system_prompt_path = write_file(
        &dirs.cwd,
        ".kiana/APPEND_SYSTEM.md",
        "Project append prompt.",
    );

    let loader = new_loader(&dirs, |_| {});
    loader.reload(None).await.expect("reload");

    assert_eq!(
        loader.get_append_system_prompt(),
        vec!["Project append prompt.".to_string()]
    );
    assert_eq!(
        loader.get_append_system_prompt_sources(),
        vec![kiana_resources::loader::SystemPromptSource {
            path: s(&append_system_prompt_path)
        }]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn does_not_expose_literal_append_system_prompt_text_as_a_source() {
    let dirs = TestDirs::new();
    let loader = new_loader(&dirs, |options| {
        options.append_system_prompt = Some(vec!["Literal append prompt.".to_string()]);
    });
    loader.reload(None).await.expect("reload");

    assert_eq!(
        loader.get_append_system_prompt(),
        vec!["Literal append prompt.".to_string()]
    );
    assert_eq!(loader.get_append_system_prompt_sources(), Vec::new());
}

#[tokio::test(flavor = "current_thread")]
async fn only_exposes_file_backed_append_system_prompt_options_as_sources() {
    let dirs = TestDirs::new();
    let append_system_prompt_path = write_file(
        dirs.temp.path(),
        "custom-append.md",
        "Custom append prompt.",
    );

    let loader = new_loader(&dirs, |options| {
        options.append_system_prompt = Some(vec![
            s(&append_system_prompt_path),
            "Literal append prompt.".to_string(),
        ]);
    });
    loader.reload(None).await.expect("reload");

    assert_eq!(
        loader.get_append_system_prompt(),
        vec![
            "Custom append prompt.".to_string(),
            "Literal append prompt.".to_string()
        ]
    );
    assert_eq!(
        loader.get_append_system_prompt_sources(),
        vec![kiana_resources::loader::SystemPromptSource {
            path: s(&append_system_prompt_path)
        }]
    );
}

// ---------------------------------------------------------------------------
// describe("extendResources")
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn should_load_skills_and_prompts_with_extension_metadata() {
    let dirs = TestDirs::new();
    let extra_skill_dir = dirs.temp_path("extra-skills/extra-skill");
    let skill_path = write_file(
        &extra_skill_dir,
        "SKILL.md",
        "---\nname: extra-skill\ndescription: Extra skill\n---\nExtra content",
    );
    let extra_prompt_dir = dirs.temp_path("extra-prompts");
    let prompt_path = write_file(
        &extra_prompt_dir,
        "extra.md",
        "---\ndescription: Extra prompt\n---\nExtra prompt content",
    );

    let loader = new_loader(&dirs, |_| {});
    loader.reload(None).await.expect("reload");

    let metadata = kiana_core::source_info::PathMetadata {
        source: "extension:extra".to_string(),
        scope: kiana_core::source_info::SourceScope::Temporary,
        origin: kiana_core::source_info::SourceOrigin::TopLevel,
        base_dir: Some(s(&extra_skill_dir)),
        package_root: None,
    };
    loader
        .extend_resources(kiana_resources::loader::ResourceExtensionPaths {
            skill_paths: vec![kiana_resources::loader::ResourceExtensionPath {
                path: s(&extra_skill_dir),
                metadata: metadata.clone(),
            }],
            prompt_paths: vec![kiana_resources::loader::ResourceExtensionPath {
                path: s(&prompt_path),
                metadata: kiana_core::source_info::PathMetadata {
                    base_dir: Some(s(&extra_prompt_dir)),
                    ..metadata.clone()
                },
            }],
            theme_paths: Vec::new(),
        })
        .expect("extend resources");

    let skill = loader
        .get_skills()
        .skills
        .into_iter()
        .find(|skill| skill.name == "extra-skill")
        .expect("extra skill");
    assert_eq!(skill.source_info.source, "extension:extra");
    assert_eq!(skill.source_info.path, s(&skill_path));

    let prompt = loader
        .get_prompts()
        .prompts
        .into_iter()
        .find(|prompt| prompt.name == "extra")
        .expect("extra prompt");
    assert_eq!(prompt.source_info.source, "extension:extra");
    assert_eq!(prompt.source_info.path, s(&prompt_path));
}

#[tokio::test(flavor = "current_thread")]
async fn extend_resources_keeps_an_empty_extension_base_dir_like_ts() {
    // TS 的 `normalizeExtensionPaths` 用 truthiness 判断 baseDir：`""` 为 falsy，
    // metadata 原样透传（不能被解析成 loader.cwd）。
    let dirs = TestDirs::new();
    let extra_skill_dir = dirs.temp_path("empty-basedir-skill");
    write_file(
        &extra_skill_dir,
        "SKILL.md",
        "---\nname: empty-basedir\ndescription: Empty baseDir\n---\nContent",
    );

    let loader = new_loader(&dirs, |_| {});
    loader.reload(None).await.expect("reload");

    loader
        .extend_resources(kiana_resources::loader::ResourceExtensionPaths {
            skill_paths: vec![kiana_resources::loader::ResourceExtensionPath {
                path: s(&extra_skill_dir),
                metadata: kiana_core::source_info::PathMetadata {
                    source: "extension:empty-basedir".to_string(),
                    scope: kiana_core::source_info::SourceScope::Temporary,
                    origin: kiana_core::source_info::SourceOrigin::TopLevel,
                    base_dir: Some(String::new()),
                    package_root: None,
                },
            }],
            ..Default::default()
        })
        .expect("extend resources");

    let skill = loader
        .get_skills()
        .skills
        .into_iter()
        .find(|skill| skill.name == "empty-basedir")
        .expect("empty basedir skill");
    assert_eq!(skill.source_info.source, "extension:empty-basedir");
    assert_eq!(skill.source_info.base_dir.as_deref(), Some(""));
}

#[tokio::test(flavor = "current_thread")]
async fn should_load_extension_resources_returned_as_file_urls() {
    let dirs = TestDirs::new();
    let extra_skill_dir = dirs.temp_path("extra skills/file-url-skill");
    let skill_path = write_file(
        &extra_skill_dir,
        "SKILL.md",
        "---\nname: file-url-skill\ndescription: File URL skill\n---\nExtra content",
    );

    let loader = new_loader(&dirs, |_| {});
    loader.reload(None).await.expect("reload");

    let file_url = url::Url::from_directory_path(&extra_skill_dir)
        .expect("file url")
        .to_string();
    loader
        .extend_resources(kiana_resources::loader::ResourceExtensionPaths {
            skill_paths: vec![kiana_resources::loader::ResourceExtensionPath {
                path: file_url,
                metadata: kiana_core::source_info::PathMetadata {
                    source: "extension:file-url".to_string(),
                    scope: kiana_core::source_info::SourceScope::Temporary,
                    origin: kiana_core::source_info::SourceOrigin::TopLevel,
                    base_dir: Some(s(&extra_skill_dir)),
                    package_root: None,
                },
            }],
            ..Default::default()
        })
        .expect("extend resources");

    let result = loader.get_skills();
    assert_eq!(result.diagnostics, Vec::new());
    let skill = result
        .skills
        .into_iter()
        .find(|skill| skill.name == "file-url-skill")
        .expect("file url skill");
    assert_eq!(skill.file_path, s(&skill_path));
    assert_eq!(skill.source_info.source, "extension:file-url");
}

#[tokio::test(flavor = "current_thread")]
async fn should_keep_package_metadata_for_skills_prompts_and_themes() {
    let dirs = TestDirs::new();
    let package_root = dirs.agent_dir.join("npm/node_modules/metadata-pkg");
    write_file(
        &package_root,
        "package.json",
        r#"{"name":"metadata-pkg","version":"1.0.0"}"#,
    );
    write_file(
        &package_root,
        "skills/package-skill/SKILL.md",
        "---\nname: package-skill\ndescription: Package skill\n---\nPackage skill content",
    );
    write_file(
        &package_root,
        "prompts/package-prompt.md",
        "---\ndescription: Package prompt\n---\nPackage prompt content",
    );
    write_file(
        &package_root,
        "themes/package-theme.json",
        &dark_theme_with_name("package-theme"),
    );

    let extension_resource_dir = dirs.temp_path("extension-resources");
    let extension_skill_dir = extension_resource_dir.join("extension-skill");
    let extension_prompts_dir = extension_resource_dir.join("prompts");
    let extension_themes_dir = extension_resource_dir.join("themes");
    write_file(
        &extension_skill_dir,
        "SKILL.md",
        "---\nname: extension-skill\ndescription: Extension skill\n---\nExtension skill content",
    );
    write_file(
        &extension_prompts_dir,
        "extension-prompt.md",
        "---\ndescription: Extension prompt\n---\nExtension prompt content",
    );
    write_file(
        &extension_themes_dir,
        "extension.json",
        &dark_theme_with_name("extension-theme"),
    );

    let settings = SettingsManager::in_memory(Settings {
        packages: Some(vec![PackageSource::Text("npm:metadata-pkg".to_string())]),
        ..Default::default()
    });
    let loader = new_loader(&dirs, |options| {
        options.settings_manager = Some(settings);
    });
    loader.reload(None).await.expect("reload");

    let extension_metadata = kiana_core::source_info::PathMetadata {
        source: "extension:discovery".to_string(),
        scope: kiana_core::source_info::SourceScope::Temporary,
        origin: kiana_core::source_info::SourceOrigin::TopLevel,
        base_dir: None,
        package_root: None,
    };
    loader
        .extend_resources(kiana_resources::loader::ResourceExtensionPaths {
            skill_paths: vec![kiana_resources::loader::ResourceExtensionPath {
                path: s(&extension_skill_dir),
                metadata: extension_metadata.clone(),
            }],
            prompt_paths: vec![kiana_resources::loader::ResourceExtensionPath {
                path: s(&extension_prompts_dir),
                metadata: extension_metadata.clone(),
            }],
            theme_paths: vec![kiana_resources::loader::ResourceExtensionPath {
                path: s(&extension_themes_dir),
                metadata: extension_metadata.clone(),
            }],
        })
        .expect("extend resources");

    let skill = loader
        .get_skills()
        .skills
        .into_iter()
        .find(|skill| skill.name == "package-skill")
        .expect("package skill");
    assert_eq!(skill.source_info.source, "npm:metadata-pkg");
    assert_eq!(
        skill.source_info.scope,
        kiana_core::source_info::SourceScope::User
    );
    assert_eq!(
        skill.source_info.origin,
        kiana_core::source_info::SourceOrigin::Package
    );

    let prompt = loader
        .get_prompts()
        .prompts
        .into_iter()
        .find(|prompt| prompt.name == "package-prompt")
        .expect("package prompt");
    assert_eq!(prompt.source_info.source, "npm:metadata-pkg");
    assert_eq!(
        prompt.source_info.origin,
        kiana_core::source_info::SourceOrigin::Package
    );

    let theme = loader
        .get_themes()
        .themes
        .into_iter()
        .find(|theme| theme.name() == Some("package-theme"))
        .expect("package theme");
    let source_info = theme.source_info().expect("theme source info");
    assert_eq!(source_info.source, "npm:metadata-pkg");
    assert_eq!(
        source_info.origin,
        kiana_core::source_info::SourceOrigin::Package
    );

    let skill = loader
        .get_skills()
        .skills
        .into_iter()
        .find(|skill| skill.name == "extension-skill")
        .expect("extension skill");
    assert_eq!(skill.source_info.source, "extension:discovery");
    let prompt = loader
        .get_prompts()
        .prompts
        .into_iter()
        .find(|prompt| prompt.name == "extension-prompt")
        .expect("extension prompt");
    assert_eq!(prompt.source_info.source, "extension:discovery");
    let theme = loader
        .get_themes()
        .themes
        .into_iter()
        .find(|theme| theme.name() == Some("extension-theme"))
        .expect("extension theme");
    assert_eq!(
        theme.source_info().expect("theme source info").source,
        "extension:discovery"
    );
}

// ---------------------------------------------------------------------------
// describe("noSkills option")
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn should_skip_skill_discovery_when_no_skills_is_true() {
    let dirs = TestDirs::new();
    write_file(
        &dirs.agent_dir,
        "skills/test-skill.md",
        "---\nname: test-skill\ndescription: A test skill\n---\nContent",
    );

    let loader = new_loader(&dirs, |options| {
        options.no_skills = true;
    });
    loader.reload(None).await.expect("reload");

    assert_eq!(loader.get_skills().skills, Vec::new());
}

#[tokio::test(flavor = "current_thread")]
async fn should_still_load_additional_skill_paths_when_no_skills_is_true() {
    let dirs = TestDirs::new();
    let custom_skill_dir = dirs.temp_path("custom-skills");
    write_file(
        &custom_skill_dir,
        "custom.md",
        "---\nname: custom\ndescription: Custom skill\n---\nContent",
    );

    let loader = new_loader(&dirs, |options| {
        options.no_skills = true;
        options.additional_skill_paths = vec![s(&custom_skill_dir)];
    });
    loader.reload(None).await.expect("reload");

    assert!(
        loader
            .get_skills()
            .skills
            .iter()
            .any(|skill| skill.name == "custom")
    );
}

// ---------------------------------------------------------------------------
// describe("override functions")
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn should_apply_skills_override() {
    let dirs = TestDirs::new();
    let injected_skill = kiana_ext_api::system_prompt::Skill {
        name: "injected".to_string(),
        description: "Injected skill".to_string(),
        file_path: "/fake/path".to_string(),
        base_dir: "/fake".to_string(),
        source_info: kiana_core::source_info::create_synthetic_source_info(
            "/fake/path",
            kiana_core::source_info::SyntheticSourceInfoOptions {
                source: "custom".to_string(),
                ..Default::default()
            },
        ),
        disable_model_invocation: false,
    };
    let loader = new_loader(&dirs, |options| {
        options.skills_override = Some(Box::new(move |_base| {
            kiana_resources::loader::SkillsResult {
                skills: vec![injected_skill.clone()],
                diagnostics: Vec::new(),
            }
        }));
    });
    loader.reload(None).await.expect("reload");

    let skills = loader.get_skills().skills;
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "injected");
}

#[tokio::test(flavor = "current_thread")]
async fn should_apply_system_prompt_override() {
    let dirs = TestDirs::new();
    let loader = new_loader(&dirs, |options| {
        options.system_prompt_override =
            Some(Box::new(|_| Some("Custom system prompt".to_string())));
    });
    loader.reload(None).await.expect("reload");

    assert_eq!(
        loader.get_system_prompt().as_deref(),
        Some("Custom system prompt")
    );
}

// ---------------------------------------------------------------------------
// describe("extension conflict detection")
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn should_detect_tool_conflicts_between_extensions() {
    let _harness = LoaderHarness::new();
    let dirs = TestDirs::new();
    write_file(
        &dirs.agent_dir,
        "extensions/ext1/index.ts",
        &tool_extension("duplicate-tool"),
    );
    write_file(
        &dirs.agent_dir,
        "extensions/ext2/index.ts",
        &tool_extension("duplicate-tool"),
    );

    let loader = new_loader(&dirs, |_| {});
    loader.reload(None).await.expect("reload");

    let errors = loader.get_extensions().errors;
    assert!(
        errors.iter().any(
            |error| error.error.contains("duplicate-tool") && error.error.contains("conflicts")
        )
    );
}

#[tokio::test(flavor = "current_thread")]
async fn should_prefer_explicit_cli_extensions_over_discovered_extensions_when_commands_and_tools_conflict()
 {
    let _harness = LoaderHarness::new();
    let dirs = TestDirs::new();
    write_file(
        &dirs.agent_dir,
        "extensions/global.ts",
        "import { Type } from \"typebox\";\nexport default function(kiana) {\n  kiana.registerTool({ name: \"duplicate-tool\", label: \"duplicate-tool\", description: \"global tool\", parameters: Type.Object({}), execute: async () => ({ content: [] }) });\n  kiana.registerCommand(\"deploy\", { description: \"global command\", handler: async () => {} });\n}\n",
    );
    let explicit_ext_path = dirs.temp_path("explicit-extension.ts");
    write_file(
        dirs.temp.path(),
        "explicit-extension.ts",
        "import { Type } from \"typebox\";\nexport default function(kiana) {\n  kiana.registerTool({ name: \"duplicate-tool\", label: \"duplicate-tool\", description: \"explicit tool\", parameters: Type.Object({}), execute: async () => ({ content: [] }) });\n  kiana.registerCommand(\"deploy\", { description: \"explicit command\", handler: async () => {} });\n}\n",
    );

    let loader = new_loader(&dirs, |options| {
        options.additional_extension_paths = vec![s(&explicit_ext_path)];
    });
    loader.reload(None).await.expect("reload");

    let result = loader.get_extensions();
    assert_eq!(result.extensions[0].path, s(&explicit_ext_path));
    assert_eq!(
        result.extensions[0].commands["deploy"]
            .description
            .as_deref(),
        Some("explicit command")
    );
    assert_eq!(
        result.extensions[1].commands["deploy"]
            .description
            .as_deref(),
        Some("global command")
    );
    assert_eq!(
        result.extensions[0].tools["duplicate-tool"]
            .definition
            .description,
        "explicit tool"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn should_leave_out_replaceable_extensions_whose_names_another_extension_registers() {
    let _harness = LoaderHarness::new();
    let dirs = TestDirs::new();
    let other_mcp_path = write_file(
        &dirs.agent_dir,
        "extensions/other-mcp.ts",
        &command_extension("mcp", "other mcp"),
    );

    let loader = new_loader(&dirs, |options| {
        options.extension_factories = vec![
            InlineExtension::Named {
                name: "mcp".to_string(),
                factory: command_factory("mcp", "built-in mcp"),
                hidden: None,
                replaceable: Some(true),
                builtin: None,
            },
            InlineExtension::Named {
                name: "llama".to_string(),
                factory: command_factory("llama", "built-in llama"),
                hidden: None,
                replaceable: Some(true),
                builtin: None,
            },
        ];
    });
    loader.reload(None).await.expect("reload");

    let result = loader.get_extensions();
    assert_eq!(
        result
            .extensions
            .iter()
            .map(|extension| extension.path.clone())
            .collect::<Vec<_>>(),
        vec![s(&other_mcp_path), "<inline:llama>".to_string()]
    );
    assert_eq!(result.errors, Vec::new());
    let other_mcp = &result.extensions[0];
    assert_eq!(
        other_mcp.commands["mcp"].description.as_deref(),
        Some("other mcp")
    );
    assert_eq!(
        result.extensions[1].commands["llama"]
            .description
            .as_deref(),
        Some("built-in llama")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn should_skip_built_in_extensions_disabled_in_settings() {
    let dirs = TestDirs::new();
    write_file(
        &dirs.agent_dir,
        "settings.json",
        r#"{"extensions":["-builtin:mcp"]}"#,
    );
    let loaded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let loader = new_loader(&dirs, |options| {
        options.extension_factories = vec![
            InlineExtension::Named {
                name: "mcp".to_string(),
                factory: recording_factory("mcp", Arc::clone(&loaded)),
                hidden: None,
                replaceable: None,
                builtin: Some(true),
            },
            InlineExtension::Named {
                name: "llama".to_string(),
                factory: recording_factory("llama", Arc::clone(&loaded)),
                hidden: None,
                replaceable: None,
                builtin: Some(true),
            },
        ];
    });
    loader.reload(None).await.expect("reload");

    let result = loader.get_extensions();
    assert_eq!(
        result
            .extensions
            .iter()
            .map(|extension| extension.path.clone())
            .collect::<Vec<_>>(),
        vec!["builtin:llama".to_string()]
    );
    assert_eq!(result.extensions[0].source_info.path, "builtin:llama");
    assert_eq!(result.extensions[0].source_info.source, "builtin");
    assert_eq!(result.extensions[0].hidden, Some(true));
    assert_eq!(&*loaded.lock().expect("lock"), &vec!["llama".to_string()]);
}

#[tokio::test(flavor = "current_thread")]
async fn should_skip_disabled_builtin_extensions_even_when_settings_or_e_enable_them() {
    let dirs = TestDirs::new();
    write_file(
        &dirs.cwd,
        ".kiana/settings.json",
        r#"{"extensions":["+builtin:mcp"]}"#,
    );
    let loaded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let loader = new_loader(&dirs, |options| {
        options.disabled_builtin_extensions = vec!["mcp".to_string()];
        options.additional_extension_paths = vec!["builtin:mcp".to_string()];
        options.extension_factories = vec![
            InlineExtension::Named {
                name: "mcp".to_string(),
                factory: recording_factory("mcp", Arc::clone(&loaded)),
                hidden: None,
                replaceable: None,
                builtin: Some(true),
            },
            InlineExtension::Named {
                name: "llama".to_string(),
                factory: recording_factory("llama", Arc::clone(&loaded)),
                hidden: None,
                replaceable: None,
                builtin: Some(true),
            },
        ];
    });
    let reload_options = ResourceLoaderReloadOptions {
        resolve_project_trust: Some(Box::new(|_| Box::pin(async { true }))),
    };
    loader.reload(Some(reload_options)).await.expect("reload");

    let result = loader.get_extensions();
    assert_eq!(
        result
            .extensions
            .iter()
            .map(|extension| extension.path.clone())
            .collect::<Vec<_>>(),
        vec!["builtin:llama".to_string()]
    );
    assert_eq!(result.errors, Vec::new());
    assert_eq!(&*loaded.lock().expect("lock"), &vec!["llama".to_string()]);
}

#[tokio::test(flavor = "current_thread")]
async fn should_load_built_in_extensions_after_file_extensions_with_and_without_trust_resolution() {
    let _harness = LoaderHarness::new();
    let dirs = TestDirs::new();
    let user_ext_path = write_file(&dirs.agent_dir, "extensions/user.ts", EMPTY_EXTENSION);
    write_file(
        &dirs.cwd,
        ".kiana/settings.json",
        r#"{"extensions":["+builtin:mcp"]}"#,
    );
    let loader = new_loader(&dirs, |options| {
        options.extension_factories = vec![InlineExtension::Named {
            name: "mcp".to_string(),
            factory: command_factory("mcp", "built-in mcp"),
            hidden: None,
            replaceable: None,
            builtin: Some(true),
        }];
    });
    let expected = vec![s(&user_ext_path), "builtin:mcp".to_string()];

    let reload_options = ResourceLoaderReloadOptions {
        resolve_project_trust: Some(Box::new(|_| Box::pin(async { true }))),
    };
    loader.reload(Some(reload_options)).await.expect("reload");
    assert_eq!(
        loader
            .get_extensions()
            .extensions
            .iter()
            .map(|extension| extension.path.clone())
            .collect::<Vec<_>>(),
        expected
    );

    loader.reload(None).await.expect("reload");
    assert_eq!(
        loader
            .get_extensions()
            .extensions
            .iter()
            .map(|extension| extension.path.clone())
            .collect::<Vec<_>>(),
        expected
    );
}

#[tokio::test(flavor = "current_thread")]
async fn should_disable_built_in_extensions_with_no_extensions_unless_loaded_with_e_builtin_name() {
    let dirs = TestDirs::new();
    let loaded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let loader = new_loader(&dirs, |options| {
        options.no_extensions = true;
        options.additional_extension_paths =
            vec!["builtin:mcp".to_string(), "builtin:missing".to_string()];
        options.extension_factories = vec![
            InlineExtension::Named {
                name: "mcp".to_string(),
                factory: recording_factory("mcp", Arc::clone(&loaded)),
                hidden: None,
                replaceable: None,
                builtin: Some(true),
            },
            InlineExtension::Named {
                name: "llama".to_string(),
                factory: recording_factory("llama", Arc::clone(&loaded)),
                hidden: None,
                replaceable: None,
                builtin: Some(true),
            },
        ];
    });
    loader.reload(None).await.expect("reload");

    let result = loader.get_extensions();
    assert_eq!(
        result
            .extensions
            .iter()
            .map(|extension| extension.path.clone())
            .collect::<Vec<_>>(),
        vec!["builtin:mcp".to_string()]
    );
    assert_eq!(
        result
            .errors
            .iter()
            .map(|error| (error.path.clone(), error.error.clone()))
            .collect::<Vec<_>>(),
        vec![(
            "builtin:missing".to_string(),
            "Unknown built-in extension: builtin:missing".to_string()
        )]
    );
    assert_eq!(&*loaded.lock().expect("lock"), &vec!["mcp".to_string()]);
}

#[tokio::test(flavor = "current_thread")]
async fn should_apply_project_built_in_extension_overrides_after_trust_resolves() {
    let dirs = TestDirs::new();
    write_file(
        &dirs.agent_dir,
        "settings.json",
        r#"{"extensions":["-builtin:mcp"]}"#,
    );
    write_file(
        &dirs.cwd,
        ".kiana/settings.json",
        r#"{"extensions":["+builtin:mcp","-builtin:llama"]}"#,
    );
    let loaded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let loader = new_loader(&dirs, |options| {
        options.extension_factories = vec![
            InlineExtension::Named {
                name: "mcp".to_string(),
                factory: recording_factory("mcp", Arc::clone(&loaded)),
                hidden: None,
                replaceable: None,
                builtin: Some(true),
            },
            InlineExtension::Named {
                name: "plain".to_string(),
                factory: recording_factory("plain", Arc::clone(&loaded)),
                hidden: None,
                replaceable: None,
                builtin: None,
            },
            InlineExtension::Named {
                name: "llama".to_string(),
                factory: recording_factory("llama", Arc::clone(&loaded)),
                hidden: None,
                replaceable: None,
                builtin: Some(true),
            },
        ];
    });
    let pre_trust_paths: Rc<RefCell<Option<Vec<String>>>> = Rc::new(RefCell::new(None));
    let paths_in = Rc::clone(&pre_trust_paths);
    let reload_options = ResourceLoaderReloadOptions {
        resolve_project_trust: Some(Box::new(move |result: LoadExtensionsResult| {
            *paths_in.borrow_mut() = Some(
                result
                    .extensions
                    .iter()
                    .map(|extension| extension.path.clone())
                    .collect(),
            );
            Box::pin(async { true })
        })),
    };
    loader.reload(Some(reload_options)).await.expect("reload");

    // 内置扩展等项目的设置确定后再加载。
    assert_eq!(
        pre_trust_paths.borrow().clone(),
        Some(vec!["<inline:plain>".to_string()])
    );
    assert_eq!(
        loader
            .get_extensions()
            .extensions
            .iter()
            .map(|extension| extension.path.clone())
            .collect::<Vec<_>>(),
        vec!["builtin:mcp".to_string(), "<inline:plain>".to_string()]
    );
    assert_eq!(
        &*loaded.lock().expect("lock"),
        &vec!["plain".to_string(), "mcp".to_string()]
    );
}

// ---------------------------------------------------------------------------
// describe("loadProjectContextFiles - nested worktree dedup")
// ---------------------------------------------------------------------------

fn link_worktree(main_dir: &Path, worktree_dir: &Path, name: &str) {
    let git_dir = main_dir.join(".git/worktrees").join(name);
    std::fs::create_dir_all(&git_dir).expect("worktree git dir");
    write_file(&main_dir.join(".git"), "HEAD", "ref: refs/heads/main\n");
    write_file(&git_dir, "HEAD", "ref: refs/heads/feat\n");
    write_file(&git_dir, "commondir", "../..");
    write_file(worktree_dir, ".git", &format!("gitdir: {}\n", s(&git_dir)));
}

struct NestedWorktree {
    outer: std::path::PathBuf,
    main: std::path::PathBuf,
    worktree: std::path::PathBuf,
    worktree_src: std::path::PathBuf,
}

fn setup_nested_worktree(dirs: &TestDirs) -> NestedWorktree {
    let outer = dirs.temp_path("outer");
    let main = outer.join("main");
    let worktree = main.join("worktrees/feat");
    let worktree_src = worktree.join("src");
    std::fs::create_dir_all(&worktree_src).expect("worktree src");
    link_worktree(&main, &worktree, "feat");
    NestedWorktree {
        outer,
        main,
        worktree,
        worktree_src,
    }
}

#[test]
fn should_skip_the_main_repos_duplicate_when_the_worktree_root_has_its_own_context() {
    let dirs = TestDirs::new();
    let fixture = setup_nested_worktree(&dirs);
    write_file(&fixture.main, "AGENTS.md", "main repo instructions");
    write_file(&fixture.worktree, "AGENTS.md", "worktree instructions");

    let files = load_project_context_files(&s(&fixture.worktree_src), &s(&dirs.agent_dir));
    assert_eq!(
        files
            .iter()
            .map(|file| file.content.clone())
            .collect::<Vec<_>>(),
        vec!["worktree instructions".to_string()]
    );
}

#[test]
fn should_load_the_worktrees_context_once_when_it_symlinks_to_the_main_repos_file() {
    let dirs = TestDirs::new();
    let fixture = setup_nested_worktree(&dirs);
    write_file(&fixture.main, "AGENTS.md", "main repo instructions");
    std::os::unix::fs::symlink(
        fixture.main.join("AGENTS.md"),
        fixture.worktree.join("AGENTS.md"),
    )
    .expect("symlink agents");

    let files = load_project_context_files(&s(&fixture.worktree_src), &s(&dirs.agent_dir));
    assert_eq!(
        files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>(),
        vec![s(&fixture.worktree.join("AGENTS.md"))]
    );
}

#[test]
fn should_skip_the_main_repos_duplicate_when_the_main_repos_file_is_a_symlink() {
    let dirs = TestDirs::new();
    let fixture = setup_nested_worktree(&dirs);
    let shared = fixture.outer.join("shared-agents.md");
    write_file(&fixture.outer, "shared-agents.md", "shared instructions");
    std::os::unix::fs::symlink(&shared, fixture.main.join("AGENTS.md")).expect("main symlink");
    std::os::unix::fs::symlink(&shared, fixture.worktree.join("AGENTS.md"))
        .expect("worktree symlink");

    let files = load_project_context_files(&s(&fixture.worktree_src), &s(&dirs.agent_dir));
    assert_eq!(
        files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>(),
        vec![s(&fixture.worktree.join("AGENTS.md"))]
    );
}

#[test]
fn should_still_inherit_the_main_repos_context_when_the_worktree_root_has_none() {
    let dirs = TestDirs::new();
    let fixture = setup_nested_worktree(&dirs);
    write_file(&fixture.main, "AGENTS.md", "main repo instructions");

    let files = load_project_context_files(&s(&fixture.worktree_src), &s(&dirs.agent_dir));
    assert_eq!(
        files
            .iter()
            .map(|file| file.content.clone())
            .collect::<Vec<_>>(),
        vec!["main repo instructions".to_string()]
    );
}

#[test]
fn should_only_skip_the_same_filename_not_a_differently_named_context_file() {
    let dirs = TestDirs::new();
    let fixture = setup_nested_worktree(&dirs);
    write_file(&fixture.main, "CLAUDE.md", "main repo instructions");
    write_file(&fixture.worktree, "AGENTS.md", "worktree instructions");

    let files = load_project_context_files(&s(&fixture.worktree_src), &s(&dirs.agent_dir));
    assert_eq!(
        files
            .iter()
            .map(|file| file.content.clone())
            .collect::<Vec<_>>(),
        vec![
            "main repo instructions".to_string(),
            "worktree instructions".to_string()
        ]
    );
}

#[test]
fn should_not_skip_the_containers_context_in_a_bare_layout() {
    let dirs = TestDirs::new();
    let proj = dirs.temp_path("proj");
    let bare = proj.join(".bare");
    let worktree = proj.join("main");
    let worktree_git_dir = bare.join("worktrees/main");
    std::fs::create_dir_all(&worktree_git_dir).expect("bare worktree git dir");
    std::fs::create_dir_all(&worktree).expect("worktree dir");
    write_file(&bare, "HEAD", "ref: refs/heads/main\n");
    write_file(&worktree_git_dir, "HEAD", "ref: refs/heads/main\n");
    write_file(&worktree_git_dir, "commondir", "../..");
    write_file(
        &worktree,
        ".git",
        &format!("gitdir: {}\n", s(&worktree_git_dir)),
    );
    write_file(&proj, "AGENTS.md", "container instructions");
    write_file(&worktree, "AGENTS.md", "worktree instructions");

    let files = load_project_context_files(&s(&worktree), &s(&dirs.agent_dir));
    assert_eq!(
        files
            .iter()
            .map(|file| file.content.clone())
            .collect::<Vec<_>>(),
        vec![
            "container instructions".to_string(),
            "worktree instructions".to_string()
        ]
    );
}

#[test]
fn should_keep_loading_ancestors_above_the_main_repo() {
    let dirs = TestDirs::new();
    let fixture = setup_nested_worktree(&dirs);
    write_file(&fixture.outer, "AGENTS.md", "outer instructions");
    write_file(&fixture.main, "AGENTS.md", "main repo instructions");
    write_file(&fixture.worktree, "AGENTS.md", "worktree instructions");

    let files = load_project_context_files(&s(&fixture.worktree_src), &s(&dirs.agent_dir));
    assert_eq!(
        files
            .iter()
            .map(|file| file.content.clone())
            .collect::<Vec<_>>(),
        vec![
            "outer instructions".to_string(),
            "worktree instructions".to_string()
        ]
    );
}

#[test]
fn should_not_skip_anything_for_a_sibling_worktree() {
    let dirs = TestDirs::new();
    let outer = dirs.temp_path("outer");
    let main = outer.join("main");
    let sib = outer.join("sib-feat");
    let sib_src = sib.join("src");
    std::fs::create_dir_all(&sib_src).expect("sibling src");
    std::fs::create_dir_all(&main).expect("main dir");
    write_file(&outer, "AGENTS.md", "outer instructions");
    write_file(&sib, "AGENTS.md", "sibling worktree instructions");
    link_worktree(&main, &sib, "sib");

    let files = load_project_context_files(&s(&sib_src), &s(&dirs.agent_dir));
    assert_eq!(
        files
            .iter()
            .map(|file| file.content.clone())
            .collect::<Vec<_>>(),
        vec![
            "outer instructions".to_string(),
            "sibling worktree instructions".to_string()
        ]
    );
}

#[test]
fn should_not_skip_the_superprojects_context_from_inside_a_submodule() {
    let dirs = TestDirs::new();
    let sup = dirs.temp_path("super");
    let sub = sup.join("vendor/lib");
    let sub_src = sub.join("src");
    std::fs::create_dir_all(&sub_src).expect("submodule src");
    write_file(&sup, "AGENTS.md", "superproject instructions");
    write_file(&sub, "AGENTS.md", "submodule instructions");
    let sub_git_dir = sup.join(".git/modules/vendor/lib");
    std::fs::create_dir_all(&sub_git_dir).expect("submodule git dir");
    write_file(&sub_git_dir, "HEAD", "ref: refs/heads/main\n");
    write_file(&sub, ".git", &format!("gitdir: {}\n", s(&sub_git_dir)));

    let files = load_project_context_files(&s(&sub_src), &s(&dirs.agent_dir));
    assert_eq!(
        files
            .iter()
            .map(|file| file.content.clone())
            .collect::<Vec<_>>(),
        vec![
            "superproject instructions".to_string(),
            "submodule instructions".to_string()
        ]
    );
}

#[test]
fn should_keep_climbing_past_an_ordinary_repo_root() {
    let dirs = TestDirs::new();
    let outer = dirs.temp_path("outer");
    let repo = outer.join("repo");
    let leaf = repo.join("src");
    std::fs::create_dir_all(&leaf).expect("leaf");
    std::fs::create_dir_all(repo.join(".git")).expect("git dir");
    write_file(&repo.join(".git"), "HEAD", "ref: refs/heads/main\n");
    write_file(&outer, "AGENTS.md", "outer instructions");
    write_file(&repo, "AGENTS.md", "repo instructions");
    write_file(&leaf, "AGENTS.md", "leaf instructions");

    let files = load_project_context_files(&s(&leaf), &s(&dirs.agent_dir));
    assert_eq!(
        files
            .iter()
            .map(|file| file.content.clone())
            .collect::<Vec<_>>(),
        vec![
            "outer instructions".to_string(),
            "repo instructions".to_string(),
            "leaf instructions".to_string()
        ]
    );
}

#[test]
fn should_climb_normally_when_the_gitdir_target_does_not_exist() {
    let dirs = TestDirs::new();
    let repo = dirs.temp_path("corrupt");
    let src = repo.join("src");
    std::fs::create_dir_all(&src).expect("src");
    write_file(&repo, ".git", "gitdir: /nonexistent/path/worktrees/feat\n");
    write_file(&repo, "AGENTS.md", "repo instructions");
    write_file(&src, "AGENTS.md", "src instructions");

    let files = load_project_context_files(&s(&src), &s(&dirs.agent_dir));
    assert_eq!(
        files
            .iter()
            .map(|file| file.content.clone())
            .collect::<Vec<_>>(),
        vec![
            "repo instructions".to_string(),
            "src instructions".to_string()
        ]
    );
}

/// 一个最小的已加载扩展（用于在 override 里构造任意扩展集合）。
fn bare_extension(path: &str) -> kiana_ext_api::types::Extension {
    kiana_ext_api::types::Extension {
        path: path.to_string(),
        resolved_path: path.to_string(),
        hidden: None,
        replaceable: Some(false),
        source_info: kiana_core::source_info::create_synthetic_source_info(
            path,
            kiana_core::source_info::SyntheticSourceInfoOptions {
                source: "test".to_string(),
                ..Default::default()
            },
        ),
        handlers: Default::default(),
        tools: Default::default(),
        message_renderers: Default::default(),
        tool_renderers: None,
        markdown_transformer: None,
        entry_renderers: None,
        commands: Default::default(),
        flags: Default::default(),
        shortcuts: Default::default(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn extend_resources_overrides_may_call_getters_without_deadlocking() {
    let dirs = TestDirs::new();
    let skill_dir = dirs.temp_path("reentrant-skill");
    write_file(
        &skill_dir,
        "SKILL.md",
        "---\nname: reentrant-skill\ndescription: Reentrant\n---\nContent",
    );

    let slot: Rc<RefCell<Option<Rc<kiana_resources::loader::DefaultResourceLoader>>>> =
        Rc::new(RefCell::new(None));
    let slot_for_override = Rc::clone(&slot);
    let reentered = Rc::new(std::cell::Cell::new(false));
    let reentered_for_override = Rc::clone(&reentered);
    let loader = {
        let mut options = kiana_resources::loader::DefaultResourceLoaderOptions {
            cwd: s(&dirs.cwd),
            agent_dir: s(&dirs.agent_dir),
            no_extensions: true,
            no_prompt_templates: true,
            no_themes: true,
            no_context_files: true,
            ..Default::default()
        };
        options.skills_override = Some(Box::new(move |base| {
            let loader = slot_for_override
                .borrow()
                .clone()
                .expect("loader installed before extend_resources");
            // 旧实现持写锁调用 override，这里会死锁。
            let _ = loader.get_skills();
            let _ = loader.get_themes();
            reentered_for_override.set(true);
            base
        }));
        Rc::new(kiana_resources::loader::DefaultResourceLoader::new(options))
    };
    *slot.borrow_mut() = Some(Rc::clone(&loader));

    loader
        .extend_resources(kiana_resources::loader::ResourceExtensionPaths {
            skill_paths: vec![kiana_resources::loader::ResourceExtensionPath {
                path: s(&skill_dir),
                metadata: kiana_core::source_info::PathMetadata {
                    source: "extension:reentrant".to_string(),
                    scope: kiana_core::source_info::SourceScope::Temporary,
                    origin: kiana_core::source_info::SourceOrigin::TopLevel,
                    base_dir: None,
                    package_root: None,
                },
            }],
            ..Default::default()
        })
        .expect("extend resources");

    assert!(reentered.get(), "skills override must run");
    assert!(
        loader
            .get_skills()
            .skills
            .iter()
            .any(|skill| skill.name == "reentrant-skill")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn failed_reload_keeps_the_new_extensions_result_like_ts() {
    // TS 在第 20 步 applyExtensionSourceInfo 之前就赋值 `this.extensionsResult`；
    // apply 里的 statSync 失败后，实例上留下的是新结果。
    let dirs = TestDirs::new();
    let missing_path = dirs.temp_path("does-not-exist/extension.ts");
    let missing_path = s(&missing_path);

    let loader = new_loader(&dirs, |options| {
        let injected = missing_path.clone();
        options.extensions_override = Some(Box::new(move |mut base| {
            base.extensions.push(bare_extension(&injected));
            base
        }));
        options.no_skills = true;
        options.no_prompt_templates = true;
        options.no_themes = true;
        options.no_context_files = true;
    });

    let error = loader.reload(None).await.expect_err("reload must fail");
    assert!(matches!(
        error,
        kiana_resources::loader::ResourceLoaderError::Io(_)
    ));
    // TS 的 statSync 未被捕获：reload() 以 Node 的 error.message 拒绝。
    assert_eq!(
        error.to_string(),
        format!("ENOENT: no such file or directory, stat '{missing_path}'")
    );
    assert!(
        loader
            .get_extensions()
            .extensions
            .iter()
            .any(|extension| extension.path == missing_path),
        "the new extensionsResult must be installed before applyExtensionSourceInfo"
    );
}
