//! 回归（fixer, minor）：命令/技能/上下文数据来自 **live** ResourceLoader。
//!
//! TS 的 `get promptTemplates()` / `_expandSkillCommand()` / `_rebuildSystemPrompt()` 每次
//! 调用都读 `this._resourceLoader`；宿主直接调用公开的 `session.resourceLoader.reload()`
//! 之后必须看到新数据。Rust 修复前在构造期缓存快照，reload 之后仍是旧数据。
#![allow(clippy::arc_with_non_send_sync)]

mod session_support;

use std::sync::Arc;

use kiana_resources::loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader,
};
use kiana_session::sdk::{CreateAgentSessionOptions, create_agent_session};
use kiana_session_core::session_manager::SessionManager;

use session_support::{run, sdk_fixture};

#[tokio::test]
async fn reloading_the_resource_loader_directly_refreshes_prompt_templates() {
    run(async {
        let fixture = sdk_fixture(None).await;
        let prompts_dir = fixture.temp_dir.path().join("prompts");
        std::fs::create_dir_all(&prompts_dir).expect("prompts dir");
        std::fs::write(prompts_dir.join("one.md"), "# One\n\nfirst template\n")
            .expect("prompt one");

        #[allow(clippy::arc_with_non_send_sync)]
        let loader: Arc<dyn ResourceLoader> =
            Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
                cwd: fixture.cwd.clone(),
                agent_dir: fixture.agent_dir.clone(),
                settings_manager: Some(fixture.settings_manager.clone()),
                additional_prompt_template_paths: vec![prompts_dir.to_string_lossy().into_owned()],
                no_extensions: true,
                no_skills: true,
                no_themes: true,
                no_context_files: true,
                ..Default::default()
            }));
        loader.reload(None).await.expect("initial reload");

        let created = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(fixture.cwd.clone()),
            agent_dir: Some(fixture.agent_dir.clone()),
            model: fixture.model(Some("faux-1")),
            model_runtime: Some(fixture.model_runtime.clone()),
            settings_manager: Some(fixture.settings_manager.clone()),
            resource_loader: Some(Arc::clone(&loader)),
            session_manager: Some(
                SessionManager::in_memory(&fixture.cwd, None, None).expect("session manager"),
            ),
            ..Default::default()
        })
        .await
        .expect("create session");
        let session = created.session;

        let names = |session: &kiana_session::agent_session::AgentSession| -> Vec<String> {
            session
                .prompt_templates()
                .into_iter()
                .map(|template| template.name)
                .collect()
        };
        assert_eq!(names(&session), vec!["one".to_string()]);

        // 宿主直接调用公开 accessor 的 reload（不经过 AgentSession::reload）。
        std::fs::write(prompts_dir.join("two.md"), "# Two\n\nsecond template\n")
            .expect("prompt two");
        session
            .resource_loader()
            .reload(None)
            .await
            .expect("direct reload");

        assert_eq!(
            names(&session),
            vec!["one".to_string(), "two".to_string()],
            "prompt templates must be read from the live resource loader"
        );
        session.dispose();
    })
    .await;
}
