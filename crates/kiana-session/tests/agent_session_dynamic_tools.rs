//! `test/agent-session-dynamic-tools.test.ts`：动态工具注册与来源元数据。
#![allow(clippy::arc_with_non_send_sync)]

mod session_support;

use std::sync::Arc;

use kiana_ext_api::events::{ExtensionEventKind, ExtensionHandler};
use kiana_ext_api::types::{ExtensionFactory, InlineExtension, RegisterCommandOptions};
use kiana_resources::loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader,
};
use kiana_session::agent_session::AgentSession;
use kiana_session::agent_session::PromptOptions;
use kiana_session::sdk::{CreateAgentSessionOptions, create_agent_session};
use kiana_session_core::session_manager::SessionManager;
use serde_json::json;

use session_support::{install_engine, ok_tool_executor, run};

thread_local! {
    /// 命令 handler 是 `Send + Sync`，不能捕获 `Arc<dyn ExtensionApi>`（trait 不带
    /// Send/Sync）；用线程本地槽位在会话线程上取回同一个 API 对象。
    static KIANA_SLOT: std::cell::RefCell<Option<Arc<dyn kiana_ext_api::types::ExtensionApi>>> =
        const { std::cell::RefCell::new(None) };
}

fn sdk_tool(
    name: &str,
    description: &str,
    snippet: Option<&str>,
) -> kiana_extensions::ToolDefinition {
    kiana_extensions::ToolDefinition {
        name: name.to_string(),
        label: name.to_string(),
        description: description.to_string(),
        prompt_snippet: snippet.map(str::to_string),
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
        execute: Arc::new(ok_tool_executor()),
        render_call: None,
        render_result: None,
    }
}

async fn create_session(
    cwd: &str,
    agent_dir: &str,
    factories: Vec<ExtensionFactory>,
    custom_tools: Vec<kiana_extensions::ToolDefinition>,
) -> AgentSession {
    let settings_manager = kiana_config::settings::SettingsManager::in_memory(Default::default());
    let loader: Arc<dyn ResourceLoader> =
        Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
            cwd: cwd.to_string(),
            agent_dir: agent_dir.to_string(),
            settings_manager: Some(settings_manager.clone()),
            extension_factories: factories
                .into_iter()
                .map(InlineExtension::Factory)
                .collect(),
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
        settings_manager: Some(settings_manager),
        session_manager: Some(SessionManager::in_memory(cwd, None, None).expect("session")),
        resource_loader: Some(loader),
        custom_tools,
        ..Default::default()
    })
    .await
    .expect("create session");
    result.session
}

#[tokio::test]
async fn refreshes_the_tool_registry_when_tools_are_registered_after_initialization() {
    run(async {
        let _engine = install_engine();
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
        std::fs::create_dir_all(&agent_dir).expect("agent");
        let factory: ExtensionFactory = Arc::new(|kiana| {
            let kiana_for_start = Arc::clone(&kiana);
            let _ = kiana.on(
                ExtensionEventKind::SessionStart,
                ExtensionHandler::Standard(Arc::new(move |_event, _ctx| {
                    let kiana = Arc::clone(&kiana_for_start);
                    Box::pin(async move {
                        let mut tool = sdk_tool(
                            "dynamic_tool",
                            "Tool registered from session_start",
                            Some("Run dynamic test behavior"),
                        );
                        tool.prompt_guidelines = Some(vec![
                            "Use dynamic_tool when the user asks for dynamic behavior tests."
                                .to_string(),
                        ]);
                        kiana.register_tool(tool);
                        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(None)
                    })
                })),
            );
            Box::pin(async {})
        });
        let session = create_session(&cwd, &agent_dir, vec![factory], Vec::new()).await;
        assert!(
            !session
                .get_all_tools()
                .iter()
                .any(|tool| tool.name == "dynamic_tool")
        );

        session.bind_extensions(Default::default()).await;
        let all_tools = session.get_all_tools();
        let dynamic_tool = all_tools
            .iter()
            .find(|tool| tool.name == "dynamic_tool")
            .expect("dynamic tool");
        let read_tool = all_tools
            .iter()
            .find(|tool| tool.name == "read")
            .expect("read tool");
        assert_eq!(
            dynamic_tool.prompt_guidelines,
            Some(vec![
                "Use dynamic_tool when the user asks for dynamic behavior tests.".to_string()
            ])
        );
        assert_eq!(dynamic_tool.source_info.path, "<inline:1>");
        assert_eq!(dynamic_tool.source_info.source, "inline");
        assert_eq!(
            serde_json::to_value(dynamic_tool.source_info.scope).expect("scope"),
            json!("temporary")
        );
        assert_eq!(
            serde_json::to_value(dynamic_tool.source_info.origin).expect("origin"),
            json!("top-level")
        );
        assert_eq!(read_tool.source_info.path, "builtin:read");
        assert_eq!(read_tool.source_info.source, "builtin");
        assert!(
            session
                .get_active_tool_names()
                .contains(&"dynamic_tool".to_string())
        );
        assert!(
            session
                .system_prompt()
                .contains("- dynamic_tool: Run dynamic test behavior")
        );
        assert!(
            session
                .system_prompt()
                .contains("- Use dynamic_tool when the user asks for dynamic behavior tests.")
        );
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn exposes_session_state_before_custom_bash_spawn_hooks_and_supports_opting_out() {
    use indexmap::IndexMap;
    use kiana_tools::bash::{BashToolOptions, create_bash_tool_definition};
    use std::sync::Mutex;

    run(async {
        let _engine = install_engine();
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
        std::fs::create_dir_all(&agent_dir).expect("agent");
        let session_env: Arc<Mutex<Option<IndexMap<String, String>>>> = Arc::new(Mutex::new(None));
        let opted_out_env: Arc<Mutex<Option<IndexMap<String, String>>>> =
            Arc::new(Mutex::new(None));
        let hook_session_env = Arc::clone(&session_env);
        let hook_opted_out_env = Arc::clone(&opted_out_env);
        let factory_cwd = cwd.clone();
        let factory: ExtensionFactory = Arc::new(move |kiana| {
            let hook_session_env = Arc::clone(&hook_session_env);
            let mut bash = create_bash_tool_definition(
                factory_cwd.clone(),
                Some(BashToolOptions {
                    spawn_hook: Some(Arc::new(
                        move |context: kiana_tools::bash::BashSpawnContext| {
                            *hook_session_env.lock().unwrap() = Some(context.env.clone());
                            context
                        },
                    )),
                    ..Default::default()
                }),
            );
            bash.name = "bash".to_string();
            bash.label = "bash".to_string();
            kiana.register_tool(bash);

            let hook_opted_out_env = Arc::clone(&hook_opted_out_env);
            let mut opted_out = create_bash_tool_definition(
                factory_cwd.clone(),
                Some(BashToolOptions {
                    expose_session_environment: false,
                    spawn_hook: Some(Arc::new(
                        move |context: kiana_tools::bash::BashSpawnContext| {
                            *hook_opted_out_env.lock().unwrap() = Some(context.env.clone());
                            context
                        },
                    )),
                    ..Default::default()
                }),
            );
            opted_out.name = "bash_without_session_env".to_string();
            opted_out.label = "bash without session env".to_string();
            kiana.register_tool(opted_out);
            Box::pin(async {})
        });
        let session = create_session(&cwd, &agent_dir, vec![factory], Vec::new()).await;
        assert!(session.system_prompt().contains(
            "You can inspect KIANA_* environment variables for current model and session details."
        ));
        let bash = session
            .state()
            .tools
            .into_iter()
            .find(|tool| tool.name == "bash")
            .expect("bash tool");
        (bash.execute)
            .execute("bash-env", json!({"command": "printf ok"}), None, None)
            .await
            .expect("bash");
        let env = session_env.lock().unwrap().clone().expect("spawn hook env");
        assert_eq!(
            env.get("KIANA_SESSION_ID").map(String::as_str),
            Some(session.session_id().as_str())
        );
        assert_eq!(
            env.get("KIANA_SESSION_FILE").map(String::as_str),
            session.session_file().as_deref()
        );
        assert_eq!(
            env.get("KIANA_PROVIDER").map(String::as_str),
            Some("anthropic")
        );
        assert_eq!(
            env.get("KIANA_MODEL").map(String::as_str),
            Some("claude-sonnet-4-5")
        );
        assert_eq!(
            env.get("KIANA_REASONING_LEVEL").map(String::as_str),
            Some("medium")
        );

        let opted_out_bash = session
            .state()
            .tools
            .into_iter()
            .find(|tool| tool.name == "bash_without_session_env")
            .expect("opted out bash tool");
        (opted_out_bash.execute)
            .execute("bash-no-env", json!({"command": "printf ok"}), None, None)
            .await
            .expect("bash");
        let env = opted_out_env
            .lock()
            .unwrap()
            .clone()
            .expect("opted out spawn hook env");
        for key in [
            "KIANA_SESSION_ID",
            "KIANA_SESSION_FILE",
            "KIANA_PROVIDER",
            "KIANA_MODEL",
            "KIANA_REASONING_LEVEL",
        ] {
            assert!(!env.contains_key(key), "{key} must not be exposed");
        }
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn returns_source_metadata_for_sdk_custom_tools() {
    run(async {
        let _engine = install_engine();
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
        std::fs::create_dir_all(&agent_dir).expect("agent");
        let session = create_session(
            &cwd,
            &agent_dir,
            Vec::new(),
            vec![sdk_tool(
                "sdk_tool",
                "SDK custom tool registered through createAgentSession",
                None,
            )],
        )
        .await;
        let tool = session
            .get_all_tools()
            .into_iter()
            .find(|tool| tool.name == "sdk_tool")
            .expect("sdk tool");
        assert_eq!(tool.source_info.path, "<sdk:sdk_tool>");
        assert_eq!(tool.source_info.source, "sdk");
        assert!(
            session
                .get_active_tool_names()
                .contains(&"sdk_tool".to_string())
        );
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn keeps_custom_tools_active_but_omits_them_from_available_tools_without_prompt_snippet() {
    run(async {
        let _engine = install_engine();
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
        std::fs::create_dir_all(&agent_dir).expect("agent");
        let factory: ExtensionFactory = Arc::new(|kiana| {
            let kiana_for_start = Arc::clone(&kiana);
            let _ = kiana.on(
                ExtensionEventKind::SessionStart,
                ExtensionHandler::Standard(Arc::new(move |_event, _ctx| {
                    let kiana = Arc::clone(&kiana_for_start);
                    Box::pin(async move {
                        kiana.register_tool(sdk_tool(
                            "hidden_tool",
                            "Description should not appear in available tools",
                            None,
                        ));
                        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(None)
                    })
                })),
            );
            Box::pin(async {})
        });
        let session = create_session(&cwd, &agent_dir, vec![factory], Vec::new()).await;
        session.bind_extensions(Default::default()).await;
        assert!(
            session
                .get_all_tools()
                .iter()
                .any(|tool| tool.name == "hidden_tool")
        );
        assert!(
            session
                .get_active_tool_names()
                .contains(&"hidden_tool".to_string())
        );
        assert!(!session.system_prompt().contains("hidden_tool"));
        assert!(
            !session
                .system_prompt()
                .contains("Description should not appear in available tools")
        );
        session.dispose();
    })
    .await;
}

/// 回归（评审 finding #12）：TS 的 `refreshTools()` 立即重建注册表，因此同一 handler
/// 之后宿主调用 `getToolDefinition` 能看到新工具。Rust 的刷新是延迟的（重入安全），
/// `getToolDefinition` 必须像 `getAllTools` 一样先冲刷 pending 刷新。
#[tokio::test]
async fn get_tool_definition_flushes_tools_registered_by_an_extension_command() {
    run(async {
        let _engine = install_engine();
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
        std::fs::create_dir_all(&agent_dir).expect("agent");
        let factory: ExtensionFactory = Arc::new(|kiana| {
            KIANA_SLOT.with(|slot| *slot.borrow_mut() = Some(Arc::clone(&kiana)));
            kiana.register_command(
                "add-late-tool",
                RegisterCommandOptions {
                    description: None,
                    get_argument_completions: None,
                    handler: Arc::new(move |_args, _ctx| {
                        Box::pin(async move {
                            KIANA_SLOT.with(|slot| {
                                if let Some(kiana) = slot.borrow().as_ref() {
                                    kiana.register_tool(sdk_tool(
                                        "late_tool",
                                        "Registered late",
                                        None,
                                    ));
                                }
                            });
                        })
                    }),
                },
            );
            Box::pin(async {})
        });
        let session = create_session(&cwd, &agent_dir, vec![factory], Vec::new()).await;
        session.bind_extensions(Default::default()).await;
        assert!(session.get_tool_definition("late_tool").is_none());

        session
            .prompt("/add-late-tool".to_string(), PromptOptions::default())
            .await
            .expect("extension command");

        let definition = session
            .get_tool_definition("late_tool")
            .expect("late tool definition");
        assert_eq!(definition.name, "late_tool");
        assert_eq!(definition.description, "Registered late");
        session.dispose();
    })
    .await;
}
