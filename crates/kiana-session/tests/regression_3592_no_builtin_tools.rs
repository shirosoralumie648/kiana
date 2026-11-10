//! regression #3592: `noTools: "builtin"` 保留扩展工具启用。
#![allow(clippy::arc_with_non_send_sync)]

mod session_support;

use std::sync::Arc;

use kiana_ext_api::events::{ExtensionEventKind, ExtensionHandler};
use kiana_ext_api::types::{ExtensionFactory, InlineExtension};
use kiana_resources::loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader,
};
use kiana_session::agent_session::AgentSession;
use kiana_session::agent_session_services::{
    CreateAgentSessionFromServicesOptions, CreateAgentSessionServicesOptions,
    create_agent_session_from_services, create_agent_session_services,
};
use kiana_session::sdk::{CreateAgentSessionOptions, NoTools, create_agent_session};
use kiana_session_core::session_manager::SessionManager;
use serde_json::json;

use session_support::{install_engine, run};

fn dynamic_tool_factory() -> ExtensionFactory {
    Arc::new(|kiana| {
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
    })
}

async fn create_session(
    cwd: &str,
    agent_dir: &str,
    settings_manager: &kiana_config::settings::SettingsManager,
    no_tools: Option<NoTools>,
) -> AgentSession {
    let loader: Arc<dyn ResourceLoader> =
        Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
            cwd: cwd.to_string(),
            agent_dir: agent_dir.to_string(),
            settings_manager: Some(settings_manager.clone()),
            extension_factories: vec![InlineExtension::Factory(dynamic_tool_factory())],
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
        no_tools,
        ..Default::default()
    })
    .await
    .expect("create session");
    let session = result.session;
    session.bind_extensions(Default::default()).await;
    session
}

#[tokio::test]
async fn keeps_extension_tools_active_when_builtin_defaults_are_disabled() {
    run(async {
        let _engine = install_engine();
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
        std::fs::create_dir_all(&agent_dir).expect("agent");
        let settings_manager =
            kiana_config::settings::SettingsManager::in_memory(Default::default());
        let session =
            create_session(&cwd, &agent_dir, &settings_manager, Some(NoTools::Builtin)).await;
        let mut all = session
            .get_all_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        all.sort();
        assert_eq!(
            all,
            vec![
                "bash",
                "dynamic_tool",
                "edit",
                "find",
                "grep",
                "ls",
                "powershell",
                "read",
                "write"
            ]
        );
        assert_eq!(session.get_active_tool_names(), vec!["dynamic_tool"]);
        assert!(
            session
                .system_prompt()
                .contains("- dynamic_tool: Run dynamic test behavior")
        );
        assert!(!session.system_prompt().contains("- read:"));
        assert!(!session.system_prompt().contains("- bash:"));
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn still_disables_all_tools_when_no_tools_is_all() {
    run(async {
        let _engine = install_engine();
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
        std::fs::create_dir_all(&agent_dir).expect("agent");
        let settings_manager =
            kiana_config::settings::SettingsManager::in_memory(Default::default());
        let session = create_session(&cwd, &agent_dir, &settings_manager, Some(NoTools::All)).await;
        assert!(session.get_all_tools().is_empty());
        assert!(session.get_active_tool_names().is_empty());
        assert!(session.system_prompt().contains("<tools>\n(none)\n"));
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn propagates_no_tools_through_service_based_session_creation() {
    run(async {
        let _engine = install_engine();
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
        std::fs::create_dir_all(&agent_dir).expect("agent");
        let settings_manager =
            kiana_config::settings::SettingsManager::in_memory(Default::default());
        let services = create_agent_session_services(CreateAgentSessionServicesOptions {
            cwd: cwd.clone(),
            agent_dir: Some(agent_dir.clone()),
            settings_manager: Some(settings_manager),
            ..Default::default()
        })
        .await
        .expect("services");
        let created = create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
            services,
            session_manager: SessionManager::in_memory(&cwd, None, None).expect("session"),
            session_start_event: None,
            model: kiana_ai::compat::get_model("anthropic", "claude-sonnet-4-5"),
            thinking_level: None,
            scoped_models: Vec::new(),
            tools: None,
            exclude_tools: None,
            no_tools: Some(NoTools::Builtin),
            custom_tools: Vec::new(),

            loop_guard: None,
            budget_config: None,
        })
        .await
        .expect("session");
        let session = created.session;
        assert!(session.get_active_tool_names().is_empty());
        assert!(session.system_prompt().contains("<tools>\n(none)\n"));
        assert!(!session.system_prompt().contains("- read:"));
        session.dispose();
    })
    .await;
}
