//! `test/default-tools-setting.test.ts`：defaultTools 设置在会话层的用例。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
mod session_support;

use std::sync::Arc;

use kiana_ai::types::Model;
use kiana_ext_api::events::ExtensionEventKind;
use kiana_ext_api::types::{ExtensionFactory, InlineExtension};
use kiana_extensions::ToolDefinition;
use kiana_resources::loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader,
};
use kiana_session::agent_session::{AgentSession, NavigateTreeOptions, PromptOptions};
use kiana_session::agent_session_services::{
    CreateAgentSessionFromServicesOptions, CreateAgentSessionServicesOptions,
    create_agent_session_from_services, create_agent_session_services,
};
use kiana_session::sdk::{CreateAgentSessionOptions, NoTools, create_agent_session};
use kiana_session_core::session_manager::SessionManager;
use serde_json::json;

use session_support::{install_engine, run, settings};

fn anthropic_model() -> Model {
    kiana_ai::compat::get_model("anthropic", "claude-sonnet-4-5").expect("claude-sonnet-4-5")
}

fn test_tool(name: &str, description: &str, default_active: Option<bool>) -> ToolDefinition {
    let mut definition = common::simple_tool_definition(name, description, None);
    definition.default_active = default_active;
    definition
}

fn tool_factory(tools: Vec<ToolDefinition>) -> ExtensionFactory {
    Arc::new(move |kiana| {
        for tool in &tools {
            kiana.register_tool(tool.clone());
        }
        Box::pin(async {})
    })
}

async fn loader(
    cwd: &str,
    agent_dir: &str,
    settings_manager: &kiana_config::settings::SettingsManager,
    factories: Vec<ExtensionFactory>,
) -> Arc<dyn ResourceLoader> {
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
    loader
}

#[derive(Default)]
struct ToolOptions {
    tools: Option<Vec<String>>,
    exclude_tools: Option<Vec<String>>,
    no_tools: Option<NoTools>,
    custom_tools: Vec<ToolDefinition>,
}

async fn create_session_with_settings(
    settings_manager: kiana_config::settings::SettingsManager,
    cwd: String,
    agent_dir: String,
    factories: Vec<ExtensionFactory>,
    options: ToolOptions,
    session_manager: Option<SessionManager>,
) -> AgentSession {
    let loader = loader(&cwd, &agent_dir, &settings_manager, factories).await;
    let result = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()),
        agent_dir: Some(agent_dir.clone()),
        model: Some(anthropic_model()),
        settings_manager: Some(settings_manager),
        session_manager: Some(
            session_manager
                .unwrap_or_else(|| SessionManager::in_memory(&cwd, None, None).expect("session")),
        ),
        resource_loader: Some(loader),
        tools: options.tools,
        exclude_tools: options.exclude_tools,
        no_tools: options.no_tools,
        custom_tools: options.custom_tools,
        ..Default::default()
    })
    .await
    .expect("create session");
    result.session
}

async fn create_session(
    default_tools: Vec<String>,
    options: ToolOptions,
    factories: Vec<ExtensionFactory>,
) -> AgentSession {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    // 目录必须活到会话结束：泄漏一个非临时目录（测试进程短命）。
    let base = Box::leak(Box::new(temp_dir));
    let cwd = base.path().to_string_lossy().into_owned();
    let agent_dir = base.path().join("agent").to_string_lossy().into_owned();
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    let settings_manager = kiana_config::settings::SettingsManager::in_memory(settings(
        json!({ "defaultTools": default_tools }),
    ));
    create_session_with_settings(settings_manager, cwd, agent_dir, factories, options, None).await
}

fn active_names(session: &AgentSession) -> Vec<String> {
    let mut names = session.get_active_tool_names();
    names.sort();
    names
}

#[tokio::test]
async fn uses_the_configured_list_as_the_initial_builtin_selection() {
    run(async {
        let _engine = install_engine();
        let session = create_session(
            vec!["grep".into(), "find".into()],
            ToolOptions::default(),
            vec![],
        )
        .await;
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
                "edit",
                "find",
                "grep",
                "ls",
                "powershell",
                "read",
                "write"
            ]
        );
        assert_eq!(session.get_active_tool_names(), vec!["grep", "find"]);
        assert!(session.system_prompt().contains("- grep:"));
        assert!(!session.system_prompt().contains("- read:"));
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn can_select_powershell_instead_of_bash() {
    run(async {
        let _engine = install_engine();
        let session = create_session(
            vec![
                "read".into(),
                "powershell".into(),
                "edit".into(),
                "write".into(),
            ],
            ToolOptions::default(),
            vec![],
        )
        .await;
        assert_eq!(
            session.get_active_tool_names(),
            vec!["read", "powershell", "edit", "write"]
        );
        assert!(
            session
                .system_prompt()
                .contains("- powershell: Execute PowerShell commands")
        );
        assert!(!session.system_prompt().contains("- bash:"));
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn activates_an_inactive_extension_tool_with_plus_name() {
    run(async {
        let _engine = install_engine();
        let session = create_session(
            vec!["+inactive_tool".into(), "-write".into()],
            ToolOptions::default(),
            vec![tool_factory(vec![test_tool(
                "inactive_tool",
                "Extension tool registered inactive",
                Some(false),
            )])],
        )
        .await;
        assert_eq!(
            active_names(&session),
            vec!["bash", "edit", "inactive_tool", "read"]
        );
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn keeps_extension_and_sdk_custom_tools_enabled() {
    run(async {
        let _engine = install_engine();
        let dynamic_factory: ExtensionFactory = Arc::new(|kiana| {
            kiana.register_tool(test_tool("static_tool", "Static tool", None));
            let kiana_for_start = Arc::clone(&kiana);
            let _ = kiana.on(
                ExtensionEventKind::SessionStart,
                kiana_ext_api::events::ExtensionHandler::Standard(Arc::new(move |_event, _ctx| {
                    let kiana = Arc::clone(&kiana_for_start);
                    Box::pin(async move {
                        kiana.register_tool(test_tool(
                            "dynamic_tool",
                            "Dynamically registered extension tool",
                            None,
                        ));
                        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(None)
                    })
                })),
            );
            Box::pin(async {})
        });
        let session = create_session(
            vec!["grep".into()],
            ToolOptions {
                custom_tools: vec![test_tool("sdk_tool", "SDK custom tool", None)],
                ..Default::default()
            },
            vec![dynamic_factory],
        )
        .await;
        session.bind_extensions(Default::default()).await;
        let names = active_names(&session);
        assert_eq!(
            names,
            vec!["dynamic_tool", "grep", "sdk_tool", "static_tool"]
        );
        let all: Vec<String> = session
            .get_all_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        for expected in ["read", "dynamic_tool", "sdk_tool", "static_tool"] {
            assert!(
                all.contains(&expected.to_string()),
                "getAllTools must contain {expected}"
            );
        }
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn preserves_explicit_tool_option_precedence() {
    run(async {
        let _engine = install_engine();
        let allowlisted = create_session(
            vec!["grep".into()],
            ToolOptions {
                tools: Some(vec!["read".into()]),
                ..Default::default()
            },
            vec![],
        )
        .await;
        assert_eq!(allowlisted.get_active_tool_names(), vec!["read"]);
        allowlisted.dispose();
        let excluded = create_session(
            vec!["read".into(), "grep".into()],
            ToolOptions {
                exclude_tools: Some(vec!["read".into()]),
                ..Default::default()
            },
            vec![],
        )
        .await;
        assert_eq!(excluded.get_active_tool_names(), vec!["grep"]);
        excluded.dispose();
        let tool_less = create_session(
            vec!["read".into()],
            ToolOptions {
                no_tools: Some(NoTools::All),
                ..Default::default()
            },
            vec![],
        )
        .await;
        assert!(tool_less.get_all_tools().is_empty());
        assert!(tool_less.get_active_tool_names().is_empty());
        tool_less.dispose();
    })
    .await;
}

#[tokio::test]
async fn applies_plus_and_minus_tool_options_to_the_default_selection() {
    run(async {
        let _engine = install_engine();
        let factories = vec![tool_factory(vec![
            test_tool("inactive_tool", "inactive", Some(false)),
            test_tool("active_tool", "active", None),
        ])];
        let session = create_session(
            vec!["+grep".into()],
            ToolOptions {
                tools: Some(vec!["+inactive_tool".into(), "-write".into()]),
                ..Default::default()
            },
            factories.clone(),
        )
        .await;
        assert_eq!(
            active_names(&session),
            vec![
                "active_tool",
                "bash",
                "edit",
                "grep",
                "inactive_tool",
                "read"
            ]
        );
        session.dispose();
        let tool_less = create_session(
            vec!["read".into()],
            ToolOptions {
                no_tools: Some(NoTools::All),
                tools: Some(vec!["+inactive_tool".into()]),
                ..Default::default()
            },
            factories,
        )
        .await;
        assert_eq!(tool_less.get_active_tool_names(), vec!["inactive_tool"]);
        tool_less.dispose();
    })
    .await;
}

#[tokio::test]
async fn rejects_invalid_tool_modifier_options() {
    run(async {
        let _engine = install_engine();
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
        std::fs::create_dir_all(&agent_dir).expect("agent");
        let settings_manager =
            kiana_config::settings::SettingsManager::in_memory(Default::default());
        let loader = loader(&cwd, &agent_dir, &settings_manager, vec![]).await;
        for (tools, expected) in [
            (
                vec!["read".to_string(), "+grep".to_string()],
                "Invalid tools option: tool names cannot be mixed with +name or -name entries",
            ),
            (
                vec!["-gr*".to_string()],
                "Invalid tools option: +name and -name entries take exact tool names, not patterns: -gr*",
            ),
        ] {
            let error = match create_agent_session(CreateAgentSessionOptions {
                cwd: Some(cwd.clone()),
                agent_dir: Some(agent_dir.clone()),
                model: Some(anthropic_model()),
                settings_manager: Some(settings_manager.clone()),
                session_manager: Some(
                    SessionManager::in_memory(&cwd, None, None).expect("session"),
                ),
                resource_loader: Some(Arc::clone(&loader)),
                tools: Some(tools),
                ..Default::default()
            })
            .await
            {
                Ok(_) => panic!("must fail"),
                Err(error) => error,
            };
            assert_eq!(error.to_string(), expected);
        }
    })
    .await;
}

#[tokio::test]
async fn applies_through_service_based_session_creation() {
    run(async {
        let _engine = install_engine();
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
        std::fs::create_dir_all(&agent_dir).expect("agent");
        let settings_manager = kiana_config::settings::SettingsManager::in_memory(settings(
            json!({ "defaultTools": ["ls"] }),
        ));
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
            model: Some(anthropic_model()),
            thinking_level: None,
            scoped_models: Vec::new(),
            tools: None,
            exclude_tools: None,
            no_tools: None,
            custom_tools: Vec::new(),

            loop_guard: None,
            budget_config: None,
        })
        .await
        .expect("session");
        let session = created.session;
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
                "edit",
                "find",
                "grep",
                "ls",
                "powershell",
                "read",
                "write"
            ]
        );
        assert_eq!(session.get_active_tool_names(), vec!["ls"]);
        session.dispose();
    })
    .await;
}

/// 记录一次 loadout 变更（`toolsAdded` / `toolsRemoved` 的 system 消息）。
fn record_loadout_change(manager: &mut SessionManager, added: &[&str], removed: &[&str]) {
    let tools_added: Option<Vec<kiana_ai::types::Tool>> = (!added.is_empty()).then(|| {
        added
            .iter()
            .map(|name| kiana_ai::types::Tool {
                name: (*name).to_string(),
                description: (*name).to_string(),
                parameters: json!({"type": "object", "properties": {}}),
                constrained_sampling: None,
            })
            .collect()
    });
    let tools_removed: Option<Vec<kiana_ai::types::ToolReference>> =
        (!removed.is_empty()).then(|| {
            removed
                .iter()
                .map(|name| kiana_ai::types::ToolReference {
                    name: (*name).to_string(),
                })
                .collect()
        });
    manager
        .append_message(kiana_agent_core::types::AgentMessage::Standard(
            kiana_ai::types::Message::System(kiana_ai::types::SystemMessage {
                role: kiana_ai::types::SystemRole::System,
                content: kiana_ai::types::SystemContent::Text(String::new()),
                sections: None,
                tools_added,
                tools_removed,
                timestamp: 0,
            }),
        ))
        .expect("append");
}

async fn resume(
    cwd: &str,
    agent_dir: &str,
    session_manager: SessionManager,
    options: ToolOptions,
    factories: Vec<ExtensionFactory>,
) -> AgentSession {
    let settings_manager = kiana_config::settings::SettingsManager::in_memory(Default::default());
    create_session_with_settings(
        settings_manager,
        cwd.to_string(),
        agent_dir.to_string(),
        factories,
        options,
        Some(session_manager),
    )
    .await
}

fn extension_tool_factories() -> Vec<ExtensionFactory> {
    vec![tool_factory(vec![
        test_tool("inactive_tool", "inactive", Some(false)),
        test_tool("active_tool", "active", None),
    ])]
}

#[tokio::test]
async fn resume_restores_the_loadout_the_transcript_records() {
    run(async {
        let _engine = install_engine();
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
        std::fs::create_dir_all(&agent_dir).expect("agent");
        let mut manager = SessionManager::in_memory(&cwd, None, None).expect("session");
        record_loadout_change(&mut manager, &["read", "grep", "active_tool"], &[]);
        record_loadout_change(&mut manager, &["inactive_tool"], &["active_tool"]);
        let session = resume(
            &cwd,
            &agent_dir,
            manager,
            ToolOptions::default(),
            extension_tool_factories(),
        )
        .await;
        assert_eq!(
            active_names(&session),
            vec!["grep", "inactive_tool", "read"]
        );
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn resume_activates_extension_tools_the_transcript_never_added_or_removed() {
    run(async {
        let _engine = install_engine();
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
        std::fs::create_dir_all(&agent_dir).expect("agent");
        let mut manager = SessionManager::in_memory(&cwd, None, None).expect("session");
        record_loadout_change(&mut manager, &["read", "grep"], &[]);
        let session = resume(
            &cwd,
            &agent_dir,
            manager,
            ToolOptions::default(),
            extension_tool_factories(),
        )
        .await;
        assert_eq!(active_names(&session), vec!["active_tool", "grep", "read"]);
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn resume_uses_explicit_tool_options_instead_of_the_recorded_loadout() {
    run(async {
        let _engine = install_engine();
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
        std::fs::create_dir_all(&agent_dir).expect("agent");
        let make = || {
            let mut manager = SessionManager::in_memory(&cwd, None, None).expect("session");
            record_loadout_change(&mut manager, &["read", "grep", "active_tool"], &[]);
            record_loadout_change(&mut manager, &["inactive_tool"], &["active_tool"]);
            manager
        };
        let allowlisted = resume(
            &cwd,
            &agent_dir,
            make(),
            ToolOptions {
                tools: Some(vec!["read".into(), "write".into()]),
                ..Default::default()
            },
            extension_tool_factories(),
        )
        .await;
        assert_eq!(allowlisted.get_active_tool_names(), vec!["read", "write"]);
        allowlisted.dispose();
        let builtinless = resume(
            &cwd,
            &agent_dir,
            make(),
            ToolOptions {
                no_tools: Some(NoTools::Builtin),
                ..Default::default()
            },
            extension_tool_factories(),
        )
        .await;
        assert_eq!(builtinless.get_active_tool_names(), vec!["active_tool"]);
        builtinless.dispose();
        let modified = resume(
            &cwd,
            &agent_dir,
            make(),
            ToolOptions {
                tools: Some(vec!["-grep".into(), "+write".into()]),
                ..Default::default()
            },
            extension_tool_factories(),
        )
        .await;
        assert_eq!(
            active_names(&modified),
            vec!["inactive_tool", "read", "write"]
        );
        modified.dispose();
    })
    .await;
}

#[tokio::test]
async fn resume_does_not_restore_excluded_tools() {
    run(async {
        let _engine = install_engine();
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
        std::fs::create_dir_all(&agent_dir).expect("agent");
        let mut manager = SessionManager::in_memory(&cwd, None, None).expect("session");
        record_loadout_change(&mut manager, &["read", "grep", "active_tool"], &[]);
        record_loadout_change(&mut manager, &["inactive_tool"], &["active_tool"]);
        let session = resume(
            &cwd,
            &agent_dir,
            manager,
            ToolOptions {
                exclude_tools: Some(vec!["inactive_tool".into()]),
                ..Default::default()
            },
            extension_tool_factories(),
        )
        .await;
        assert_eq!(active_names(&session), vec!["grep", "read"]);
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn resume_uses_default_tools_when_the_transcript_records_no_loadout() {
    run(async {
        let _engine = install_engine();
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
        std::fs::create_dir_all(&agent_dir).expect("agent");
        let mut manager = SessionManager::in_memory(&cwd, None, None).expect("session");
        manager
            .append_message(kiana_agent_core::types::AgentMessage::Standard(
                kiana_ai::types::Message::User(kiana_ai::types::UserMessage {
                    role: kiana_ai::types::UserRole::User,
                    content: kiana_ai::types::UserContent::Text("hi".to_string()),
                    timestamp: 0,
                }),
            ))
            .expect("append");
        let session = resume(
            &cwd,
            &agent_dir,
            manager,
            ToolOptions::default(),
            extension_tool_factories(),
        )
        .await;
        assert_eq!(
            active_names(&session),
            vec!["active_tool", "bash", "edit", "read", "write"]
        );
        session.dispose();
    })
    .await;
}

// ============================================================================
// reload / resume（#10245、#10785）的补充用例
// ============================================================================

/// 带可写 `settings.json` 的夹具（reload 测试用真实文件设置）。
struct FileFixture {
    _temp: tempfile::TempDir,
    cwd: String,
    agent_dir: String,
}

impl FileFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
        std::fs::create_dir_all(&agent_dir).expect("agent dir");
        FileFixture {
            _temp: temp,
            cwd,
            agent_dir,
        }
    }

    fn write_settings(&self, value: serde_json::Value) {
        std::fs::write(
            std::path::Path::new(&self.agent_dir).join("settings.json"),
            value.to_string(),
        )
        .expect("write settings");
    }

    async fn create_session(&self, options: ToolOptions) -> AgentSession {
        let settings_manager =
            kiana_config::settings::SettingsManager::create(&self.cwd, &self.agent_dir);
        let loader = loader(
            &self.cwd,
            &self.agent_dir,
            &settings_manager,
            vec![tool_factory(vec![test_tool(
                "inactive_tool",
                "Extension tool registered inactive",
                Some(false),
            )])],
        )
        .await;
        let created = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(self.cwd.clone()),
            agent_dir: Some(self.agent_dir.clone()),
            model: Some(anthropic_model()),
            settings_manager: Some(settings_manager),
            session_manager: Some(
                SessionManager::in_memory(&self.cwd, None, None).expect("session"),
            ),
            resource_loader: Some(loader),
            tools: options.tools,
            exclude_tools: options.exclude_tools,
            no_tools: options.no_tools,
            custom_tools: options.custom_tools,
            ..Default::default()
        })
        .await
        .expect("create session");
        created.session
    }
}

#[tokio::test]
async fn reload_activates_only_tools_newly_added_to_default_tools() {
    run(async {
        let _engine = install_engine();
        let fixture = FileFixture::new();
        let session = fixture.create_session(ToolOptions::default()).await;
        assert_eq!(
            session.get_active_tool_names(),
            vec!["read", "bash", "edit", "write"]
        );
        session.set_active_tools_by_name(vec![
            "read".to_string(),
            "edit".to_string(),
            "write".to_string(),
        ]);

        fixture.write_settings(json!({"defaultTools": ["+inactive_tool", "+grep"]}));
        session.reload(None).await.expect("reload");
        // bash 在会话期间被关闭且不是新增项，保持关闭。
        assert_eq!(
            active_names(&session),
            vec!["edit", "grep", "inactive_tool", "read", "write"]
        );

        // 从设置中移除工具不会禁用它们。
        fixture.write_settings(json!({"defaultTools": ["-read"]}));
        session.reload(None).await.expect("reload");
        assert_eq!(
            active_names(&session),
            vec!["edit", "grep", "inactive_tool", "read", "write"]
        );
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn reload_keeps_tools_removed_by_minus_name_tool_options_removed() {
    run(async {
        let _engine = install_engine();
        let fixture = FileFixture::new();
        fixture.write_settings(json!({"defaultTools": ["read"]}));
        let session = fixture
            .create_session(ToolOptions {
                tools: Some(vec!["-bash".into(), "+grep".into()]),
                ..Default::default()
            })
            .await;
        assert_eq!(session.get_active_tool_names(), vec!["read", "grep"]);

        fixture.write_settings(json!({"defaultTools": ["read", "bash", "inactive_tool"]}));
        session.reload(None).await.expect("reload");
        assert_eq!(
            active_names(&session),
            vec!["grep", "inactive_tool", "read"]
        );
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn reload_keeps_explicit_tool_options() {
    run(async {
        let _engine = install_engine();
        let fixture = FileFixture::new();
        let allowlisted = fixture
            .create_session(ToolOptions {
                tools: Some(vec!["read".into()]),
                ..Default::default()
            })
            .await;
        fixture.write_settings(json!({"defaultTools": ["+grep"]}));
        allowlisted.reload(None).await.expect("reload");
        assert_eq!(allowlisted.get_active_tool_names(), vec!["read"]);
        allowlisted.dispose();

        fixture.write_settings(json!({}));
        let builtinless = fixture
            .create_session(ToolOptions {
                no_tools: Some(NoTools::Builtin),
                ..Default::default()
            })
            .await;
        fixture.write_settings(json!({"defaultTools": ["+grep"]}));
        builtinless.reload(None).await.expect("reload");
        assert!(builtinless.get_active_tool_names().is_empty());
        builtinless.dispose();

        fixture.write_settings(json!({}));
        let excluded = fixture
            .create_session(ToolOptions {
                exclude_tools: Some(vec!["grep".into()]),
                ..Default::default()
            })
            .await;
        fixture.write_settings(json!({"defaultTools": ["+grep", "+inactive_tool"]}));
        excluded.reload(None).await.expect("reload");
        assert_eq!(
            active_names(&excluded),
            vec!["bash", "edit", "inactive_tool", "read", "write"]
        );
        excluded.dispose();
    })
    .await;
}

#[tokio::test]
async fn resume_keeps_tools_removed_before_a_compaction_inactive() {
    run(async {
        let _engine = install_engine();
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
        std::fs::create_dir_all(&agent_dir).expect("agent");
        let mut manager = SessionManager::in_memory(&cwd, None, None).expect("session");
        record_loadout_change(&mut manager, &["read", "grep", "active_tool"], &[]);
        record_loadout_change(&mut manager, &["inactive_tool"], &["active_tool"]);
        let kept = manager
            .append_message(kiana_agent_core::types::AgentMessage::Standard(
                kiana_ai::types::Message::User(kiana_ai::types::UserMessage {
                    role: kiana_ai::types::UserRole::User,
                    content: kiana_ai::types::UserContent::Text("hi".to_string()),
                    timestamp: 0,
                }),
            ))
            .expect("append");
        manager
            .append_compaction("summary", Some(&kept), 1000.0, None, None, None)
            .expect("compact");
        let session = resume(
            &cwd,
            &agent_dir,
            manager,
            ToolOptions::default(),
            extension_tool_factories(),
        )
        .await;
        assert_eq!(
            active_names(&session),
            vec!["grep", "inactive_tool", "read"]
        );
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn resume_restores_the_target_branch_loadout_on_tree_navigation() {
    run(async {
        let _engine = install_engine();
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
        std::fs::create_dir_all(&agent_dir).expect("agent");
        let mut manager = SessionManager::in_memory(&cwd, None, None).expect("session");
        record_loadout_change(&mut manager, &["read", "grep"], &[]);
        record_loadout_change(&mut manager, &[], &["grep"]);
        let target = manager
            .append_message(kiana_agent_core::types::AgentMessage::Standard(
                kiana_ai::types::Message::User(kiana_ai::types::UserMessage {
                    role: kiana_ai::types::UserRole::User,
                    content: kiana_ai::types::UserContent::Text("first".to_string()),
                    timestamp: 0,
                }),
            ))
            .expect("append");
        record_loadout_change(&mut manager, &["grep"], &["active_tool"]);
        let session = resume(
            &cwd,
            &agent_dir,
            manager,
            ToolOptions::default(),
            extension_tool_factories(),
        )
        .await;
        assert_eq!(active_names(&session), vec!["grep", "read"]);

        session
            .navigate_tree(
                target,
                NavigateTreeOptions {
                    summarize: Some(false),
                    ..Default::default()
                },
            )
            .await
            .expect("navigate");
        assert_eq!(active_names(&session), vec!["active_tool", "read"]);
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn resume_keeps_registered_tools_outside_the_allowlist_inactive_on_tree_navigation() {
    run(async {
        let _engine = install_engine();
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
        std::fs::create_dir_all(&agent_dir).expect("agent");
        let mut manager = SessionManager::in_memory(&cwd, None, None).expect("session");
        record_loadout_change(&mut manager, &["read"], &[]);
        let target = manager
            .append_message(kiana_agent_core::types::AgentMessage::Standard(
                kiana_ai::types::Message::User(kiana_ai::types::UserMessage {
                    role: kiana_ai::types::UserRole::User,
                    content: kiana_ai::types::UserContent::Text("first".to_string()),
                    timestamp: 0,
                }),
            ))
            .expect("append");
        let session = resume(
            &cwd,
            &agent_dir,
            manager,
            ToolOptions {
                tools: Some(vec!["read".into()]),
                custom_tools: vec![test_tool("mcp__server__lookup", "MCP tool", None)],
                ..Default::default()
            },
            extension_tool_factories(),
        )
        .await;
        assert_eq!(session.get_active_tool_names(), vec!["read"]);

        session
            .navigate_tree(
                target,
                NavigateTreeOptions {
                    summarize: Some(false),
                    ..Default::default()
                },
            )
            .await
            .expect("navigate");
        assert_eq!(session.get_active_tool_names(), vec!["read"]);
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn services_treat_an_empty_agent_dir_as_absent() {
    run(async {
        let _engine = install_engine();
        let temp = tempfile::tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let settings_manager =
            kiana_config::settings::SettingsManager::in_memory(Default::default());
        let model_runtime = kiana_config::models::runtime::ModelRuntime::create(
            kiana_config::models::runtime::CreateModelRuntimeOptions {
                models_path: kiana_config::models::runtime::ModelsPathOption::Disabled,
                allow_model_network: false,
                refresh_on_create: Some(false),
                ..Default::default()
            },
        )
        .await
        .expect("runtime");
        let services = create_agent_session_services(CreateAgentSessionServicesOptions {
            cwd: cwd.clone(),
            agent_dir: Some(String::new()),
            settings_manager: Some(settings_manager),
            model_runtime: Some(model_runtime),
            resource_loader_options: Some(DefaultResourceLoaderOptions {
                no_skills: true,
                no_prompt_templates: true,
                no_themes: true,
                no_context_files: true,
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .expect("services");
        assert_eq!(
            services.agent_dir,
            kiana_core::config::get_agent_dir()
                .to_string_lossy()
                .into_owned()
        );
    })
    .await;
}

// `PromptOptions` / `ExtensionEventKind` 仅为与 TS 文件对照保留的类型引用。
#[allow(dead_code)]
fn _unused(_: PromptOptions, _: ExtensionEventKind) {}
