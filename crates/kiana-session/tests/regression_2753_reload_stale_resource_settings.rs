//! regression #2753: reload 读取更新后的顶层 prompt 设置。
#![allow(clippy::arc_with_non_send_sync)]

mod session_support;

use std::sync::Arc;

use kiana_resources::loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader,
};
use kiana_session::sdk::{CreateAgentSessionOptions, create_agent_session};
use kiana_session_core::session_manager::SessionManager;

use session_support::{install_engine, run};

#[tokio::test]
async fn applies_updated_top_level_prompt_settings_on_reload_after_startup() {
    run(async {
        let _engine = install_engine();
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let agent_dir = temp.path().join("agent");
        std::fs::create_dir_all(agent_dir.join("prompts")).expect("prompts dir");
        std::fs::write(
            agent_dir.join("prompts").join("test.md"),
            "Echo test prompt\n",
        )
        .expect("prompt");
        let agent_dir = agent_dir.to_string_lossy().into_owned();
        let settings_manager = kiana_config::settings::SettingsManager::create(&cwd, &agent_dir);
        let loader: Arc<dyn ResourceLoader> =
            Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
                cwd: cwd.clone(),
                agent_dir: agent_dir.clone(),
                settings_manager: Some(settings_manager.clone()),
                no_skills: true,
                no_themes: true,
                ..Default::default()
            }));
        loader.reload(None).await.expect("reload");
        let result = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(cwd.clone()),
            agent_dir: Some(agent_dir.clone()),
            model: kiana_ai::compat::get_model("anthropic", "claude-sonnet-4-5"),
            settings_manager: Some(settings_manager.clone()),
            session_manager: Some(SessionManager::in_memory(&cwd, None, None).expect("session")),
            resource_loader: Some(loader),
            ..Default::default()
        })
        .await
        .expect("create session");
        let session = result.session;
        assert!(
            session
                .prompt_templates()
                .iter()
                .any(|prompt| prompt.name == "test")
        );

        std::fs::write(
            std::path::Path::new(&agent_dir).join("settings.json"),
            format!("{}\n", serde_json::json!({"prompts": ["-prompts/test.md"]})),
        )
        .expect("settings");

        session.reload(None).await.expect("reload");

        assert_eq!(
            settings_manager.get_global_settings().prompts,
            Some(vec!["-prompts/test.md".to_string()])
        );
        assert!(
            !session
                .prompt_templates()
                .iter()
                .any(|prompt| prompt.name == "test")
        );
        session.dispose();
    })
    .await;
}
