//! regression #2835: 工具允许列表过滤扩展工具。
#![allow(clippy::arc_with_non_send_sync)]

mod session_support;

use std::sync::Arc;

use kiana_ext_api::events::{ExtensionEventKind, ExtensionHandler};
use kiana_ext_api::types::{ExtensionFactory, InlineExtension};
use kiana_resources::loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader,
};
use kiana_session::sdk::{CreateAgentSessionOptions, create_agent_session};
use kiana_session_core::session_manager::SessionManager;
use serde_json::json;

use session_support::{install_engine, run};

async fn create_session(
    cwd: &str,
    agent_dir: &str,
    settings_manager: &kiana_config::settings::SettingsManager,
    tools: Option<Vec<String>>,
) -> kiana_session::agent_session::AgentSession {
    let factory: ExtensionFactory = Arc::new(|kiana| {
        let kiana_for_start = Arc::clone(&kiana);
        let _ = kiana.on(
            ExtensionEventKind::SessionStart,
            ExtensionHandler::Standard(Arc::new(move |_event, _ctx| {
                let kiana = Arc::clone(&kiana_for_start);
                Box::pin(async move {
                    kiana.register_tool(kiana_extensions::ToolDefinition {
                        name: "dynamic_tool".to_string(),
                        label: "Dynamic Tool".to_string(),
                        description: "Tool registered from session_start".to_string(),
                        prompt_snippet: Some("Run dynamic test behavior".to_string()),
                        prompt_guidelines: None,
                        parameters: json!({"type": "object", "properties": {}}),
                        constrained_sampling: None,
                        render_shell: None,
                        prepare_arguments: None,
                        output_schema: None,
                        exposure: None,
                        namespace: None,
                        reaches_tools: None,
                        annotations: None,
                        default_active: None,
                        prepare_loadout: None,
                        execution_mode: None,
                        execute: Arc::new(session_support::ok_tool_executor()),
                        render_call: None,
                        render_result: None,
                    });
                    Ok::<_, Box<dyn std::error::Error + Send + Sync>>(None)
                })
            })),
        );
        Box::pin(async {})
    });
    let loader: Arc<dyn ResourceLoader> =
        Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
            cwd: cwd.to_string(),
            agent_dir: agent_dir.to_string(),
            settings_manager: Some(settings_manager.clone()),
            extension_factories: vec![InlineExtension::Factory(factory)],
            no_skills: true,
            no_prompt_templates: true,
            no_themes: true,
            no_context_files: true,
            ..Default::default()
        }));
    loader.reload(None).await.expect("reload");
    let result = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.to_string()),
        agent_dir: Some(agent_dir.to_string()),
        model: kiana_ai::compat::get_model("anthropic", "claude-sonnet-4-5"),
        settings_manager: Some(settings_manager.clone()),
        session_manager: Some(SessionManager::in_memory(cwd, None, None).expect("session")),
        resource_loader: Some(loader),
        tools,
        ..Default::default()
    })
    .await
    .expect("create session");
    let session = result.session;
    session.bind_extensions(Default::default()).await;
    session
}

#[tokio::test]
async fn allows_only_explicitly_listed_builtin_and_extension_tools() {
    run(async {
        let _engine = install_engine();
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
        std::fs::create_dir_all(&agent_dir).expect("agent");
        let settings_manager =
            kiana_config::settings::SettingsManager::in_memory(Default::default());
        let session = create_session(
            &cwd,
            &agent_dir,
            &settings_manager,
            Some(vec!["read".to_string(), "dynamic_tool".to_string()]),
        )
        .await;
        let mut all = session
            .get_all_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        all.sort();
        assert_eq!(all, vec!["dynamic_tool", "read"]);
        let mut active = session.get_active_tool_names();
        active.sort();
        assert_eq!(active, vec!["dynamic_tool", "read"]);
        assert!(
            session
                .system_prompt()
                .contains("- read: Read file contents")
        );
        assert!(
            session
                .system_prompt()
                .contains("- dynamic_tool: Run dynamic test behavior")
        );
        assert!(!session.system_prompt().contains("- bash:"));
        assert!(!session.system_prompt().contains("- edit:"));
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn disables_all_tools_when_the_allowlist_is_empty() {
    run(async {
        let _engine = install_engine();
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
        std::fs::create_dir_all(&agent_dir).expect("agent");
        let settings_manager =
            kiana_config::settings::SettingsManager::in_memory(Default::default());
        let session = create_session(&cwd, &agent_dir, &settings_manager, Some(Vec::new())).await;
        assert!(session.get_all_tools().is_empty());
        assert!(session.get_active_tool_names().is_empty());
        assert!(session.system_prompt().contains("<tools>\n(none)\n"));
        assert!(!session.system_prompt().contains("dynamic_tool"));
        session.dispose();
    })
    .await;
}
