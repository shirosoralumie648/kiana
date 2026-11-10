//! `test/sdk-skills.test.ts`：resourceLoader 的技能暴露。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
mod session_support;

use std::sync::Arc;

use kiana_ai::types::Model;
use kiana_resources::loader::{ResourceLoader, SkillsResult};
use kiana_session::sdk::{CreateAgentSessionOptions, create_agent_session};
use kiana_session_core::session_manager::SessionManager;

use session_support::{run, sdk_fixture};

#[tokio::test]
async fn discovers_skills_by_default_and_exposes_them_on_the_session() {
    run(async {
        let _engine = session_support::install_engine();
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let skills_dir = temp_dir.path().join("skills").join("test-skill");
        std::fs::create_dir_all(&skills_dir).expect("skills dir");
        std::fs::write(
            skills_dir.join("SKILL.md"),
            "---\nname: test-skill\ndescription: A test skill for SDK tests.\n---\n\n# Test Skill\n\nThis is a test skill.\n",
        )
        .expect("skill file");
        let temp = temp_dir.path().to_string_lossy().into_owned();
        let model = kiana_ai::compat::get_model("anthropic", "claude-sonnet-4-5").expect("model");
        let result = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(temp.clone()),
            agent_dir: Some(temp.clone()),
            model: Some(model),
            session_manager: Some(
                SessionManager::in_memory(&temp, None, None).expect("session"),
            ),
            ..Default::default()
        })
        .await
        .expect("create session");
        let session = result.session;
        let skills = session.resource_loader().get_skills();
        assert!(!skills.skills.is_empty());
        assert!(skills.skills.iter().any(|skill| skill.name == "test-skill"));
        session.dispose();
    })
    .await;
}

/// 与 common 的 TestResourceLoader 等价的空技能加载器。
struct EmptySkillsLoader {
    extensions: kiana_extensions::LoadExtensionsResult,
}

impl ResourceLoader for EmptySkillsLoader {
    fn get_extensions(&self) -> kiana_extensions::LoadExtensionsResult {
        self.extensions.clone()
    }
    fn get_skills(&self) -> SkillsResult {
        SkillsResult::default()
    }
    fn get_prompts(&self) -> kiana_resources::loader::PromptsResult {
        kiana_resources::loader::PromptsResult::default()
    }
    fn get_themes(&self) -> kiana_resources::loader::ThemesResult {
        kiana_resources::loader::ThemesResult::default()
    }
    fn get_agents_files(&self) -> kiana_resources::loader::AgentsFilesResult {
        kiana_resources::loader::AgentsFilesResult::default()
    }
    fn get_system_prompt(&self) -> Option<String> {
        None
    }
    fn get_system_prompt_source(&self) -> Option<kiana_resources::loader::SystemPromptSource> {
        None
    }
    fn get_append_system_prompt(&self) -> Vec<String> {
        Vec::new()
    }
    fn get_append_system_prompt_sources(&self) -> Vec<kiana_resources::loader::SystemPromptSource> {
        Vec::new()
    }
    fn extend_resources(
        &self,
        _paths: kiana_resources::loader::ResourceExtensionPaths,
    ) -> Result<(), kiana_resources::loader::ResourceLoaderError> {
        Ok(())
    }
    fn reload<'a>(
        &'a self,
        _options: Option<kiana_resources::loader::ResourceLoaderReloadOptions>,
    ) -> kiana_ext_api::LocalBoxFuture<'a, Result<(), kiana_resources::loader::ResourceLoaderError>>
    {
        Box::pin(async { Ok(()) })
    }
}

fn empty_loader() -> Arc<dyn ResourceLoader> {
    Arc::new(EmptySkillsLoader {
        extensions: kiana_extensions::LoadExtensionsResult {
            extensions: Vec::new(),
            errors: Vec::new(),
            warnings: None,
            runtime: kiana_extensions::create_extension_runtime(),
        },
    })
}

#[tokio::test]
async fn has_empty_skills_when_the_resource_loader_returns_none() {
    run(async {
        let fixture = sdk_fixture(None).await;
        let result = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(fixture.cwd.clone()),
            agent_dir: Some(fixture.agent_dir.clone()),
            model: fixture.model(Some("faux-1")),
            model_runtime: Some(fixture.model_runtime.clone()),
            settings_manager: Some(fixture.settings_manager.clone()),
            session_manager: Some(
                SessionManager::in_memory(&fixture.cwd, None, None).expect("session"),
            ),
            resource_loader: Some(empty_loader()),
            ..Default::default()
        })
        .await
        .expect("create session");
        let session = result.session;
        assert!(session.resource_loader().get_skills().skills.is_empty());
        assert!(
            session
                .resource_loader()
                .get_skills()
                .diagnostics
                .is_empty()
        );
        session.dispose();
        fixture.cleanup();
    })
    .await;
}

#[tokio::test]
async fn uses_provided_skills_when_the_resource_loader_supplies_them() {
    run(async {
        let fixture = sdk_fixture(None).await;
        let skill = kiana_ext_api::system_prompt::Skill {
            name: "custom-skill".to_string(),
            description: "A custom skill".to_string(),
            file_path: "/fake/path/SKILL.md".to_string(),
            base_dir: "/fake/path".to_string(),
            source_info: kiana_core::source_info::create_synthetic_source_info(
                "/fake/path/SKILL.md",
                kiana_core::source_info::SyntheticSourceInfoOptions {
                    source: "sdk".to_string(),
                    scope: None,
                    origin: None,
                    base_dir: None,
                },
            ),
            disable_model_invocation: false,
        };
        struct SkillsLoader {
            extensions: kiana_extensions::LoadExtensionsResult,
            skill: kiana_ext_api::system_prompt::Skill,
        }
        impl ResourceLoader for SkillsLoader {
            fn get_extensions(&self) -> kiana_extensions::LoadExtensionsResult {
                self.extensions.clone()
            }
            fn get_skills(&self) -> SkillsResult {
                SkillsResult {
                    skills: vec![self.skill.clone()],
                    diagnostics: Vec::new(),
                }
            }
            fn get_prompts(&self) -> kiana_resources::loader::PromptsResult {
                kiana_resources::loader::PromptsResult::default()
            }
            fn get_themes(&self) -> kiana_resources::loader::ThemesResult {
                kiana_resources::loader::ThemesResult::default()
            }
            fn get_agents_files(&self) -> kiana_resources::loader::AgentsFilesResult {
                kiana_resources::loader::AgentsFilesResult::default()
            }
            fn get_system_prompt(&self) -> Option<String> {
                None
            }
            fn get_system_prompt_source(
                &self,
            ) -> Option<kiana_resources::loader::SystemPromptSource> {
                None
            }
            fn get_append_system_prompt(&self) -> Vec<String> {
                Vec::new()
            }
            fn get_append_system_prompt_sources(
                &self,
            ) -> Vec<kiana_resources::loader::SystemPromptSource> {
                Vec::new()
            }
            fn extend_resources(
                &self,
                _paths: kiana_resources::loader::ResourceExtensionPaths,
            ) -> Result<(), kiana_resources::loader::ResourceLoaderError> {
                Ok(())
            }
            fn reload<'a>(
                &'a self,
                _options: Option<kiana_resources::loader::ResourceLoaderReloadOptions>,
            ) -> kiana_ext_api::LocalBoxFuture<
                'a,
                Result<(), kiana_resources::loader::ResourceLoaderError>,
            > {
                Box::pin(async { Ok(()) })
            }
        }
        let loader: Arc<dyn ResourceLoader> = Arc::new(SkillsLoader {
            extensions: kiana_extensions::LoadExtensionsResult {
                extensions: Vec::new(),
                errors: Vec::new(),
                warnings: None,
                runtime: kiana_extensions::create_extension_runtime(),
            },
            skill,
        });
        let result = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(fixture.cwd.clone()),
            agent_dir: Some(fixture.agent_dir.clone()),
            model: fixture.model(Some("faux-1")),
            model_runtime: Some(fixture.model_runtime.clone()),
            settings_manager: Some(fixture.settings_manager.clone()),
            session_manager: Some(
                SessionManager::in_memory(&fixture.cwd, None, None).expect("session"),
            ),
            resource_loader: Some(loader),
            ..Default::default()
        })
        .await
        .expect("create session");
        let session = result.session;
        let skills = session.resource_loader().get_skills();
        assert_eq!(skills.skills.len(), 1);
        assert_eq!(skills.skills[0].name, "custom-skill");
        assert!(skills.diagnostics.is_empty());
        session.dispose();
        fixture.cleanup();
    })
    .await;
}

#[allow(dead_code)]
fn _unused(_: Model) {}
