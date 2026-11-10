//! `interactive-mode-status.test.ts` 的 `showLoadedResources` 部分（§2.50.21）。
//!
//! 覆盖：折叠 / 展开列表、verbose、扩展名缩写、scope 分组顺序、context 文件顺序、
//! 同 index 扩展逐级消歧、npm 兄弟扩展、诊断分组与 Windows 路径。

#![allow(
    clippy::await_holding_lock,
    reason = "主题初始化是进程级状态：同文件的用例必须串行（TS 的 beforeAll(initTheme) 等价）"
)]

mod common;

use std::sync::Arc;
use std::sync::{Mutex, MutexGuard};

use kiana_ai::models::ModelsRefreshOptions;
use kiana_ai::models::provider::{CreateProviderOptions, ProviderApiImpl, create_provider};
use kiana_ai::providers::faux::{
    FauxModelDefinition, RegisterFauxProviderOptions, create_faux_core,
};
use kiana_config::auth::storage::{AuthStorage, auth_data_from_value};
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use kiana_config::settings::TuiMode;
use kiana_config::settings::schema::Settings;
use kiana_config::settings::{QuietStartup, SettingsManager};
use kiana_core::diagnostics::{
    DiagnosticKind, ResourceCollision, ResourceDiagnostic, ResourceType,
};
use kiana_core::source_info::{SourceInfo, SourceOrigin, SourceScope};
use kiana_ext_api::system_prompt::{ContextFile, Skill};
use kiana_interactive::interactive_mode::{InteractiveMode, InteractiveModeOptions};
use kiana_resources::loader::{
    AgentsFilesResult, DefaultResourceLoaderOptions, PromptsResult, SkillsResult, ThemesResult,
};
use kiana_resources::prompts::PromptTemplate;
use kiana_session::agent_session_runtime::{
    AgentSessionRuntimeError, CreateAgentSessionRuntimeFactory, CreateAgentSessionRuntimeResult,
    create_agent_session_runtime,
};
use kiana_session::agent_session_services::{
    CreateAgentSessionFromServicesOptions, CreateAgentSessionServicesOptions,
    create_agent_session_from_services, create_agent_session_services,
};
use kiana_session_core::session_manager::SessionManager;
use serde_json::json;

static THEME_LOCK: Mutex<()> = Mutex::new(());

fn theme() -> MutexGuard<'static, ()> {
    let guard = THEME_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    kiana_theme::init_theme(Some("dark"), false);
    guard
}

/// 需要注入的资源集合。
#[derive(Default, Clone)]
struct Resources {
    skills: Vec<Skill>,
    skill_diagnostics: Vec<ResourceDiagnostic>,
    prompts: Vec<PromptTemplate>,
    prompt_diagnostics: Vec<ResourceDiagnostic>,
    themes: Vec<Arc<kiana_theme::theme::Theme>>,
    theme_diagnostics: Vec<ResourceDiagnostic>,
    agents_files: Vec<ContextFile>,
    system_prompt_path: Option<String>,
    append_system_prompt_paths: Vec<String>,
}

struct Fixture {
    mode: InteractiveMode,
    _temp: tempfile::TempDir,
    _engine: common::TestEngine,
    _faux: kiana_ai::providers::faux::FauxCore,
    cwd: String,
}

async fn fixture(resources: Resources) -> Fixture {
    unsafe { std::env::set_var("KIANA_OFFLINE", "1") };
    let engine = common::install_engine();
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let cwd = temp_dir
        .path()
        .join("project")
        .to_string_lossy()
        .into_owned();
    let agent_dir = temp_dir.path().join("agent").to_string_lossy().into_owned();
    std::fs::create_dir_all(&cwd).expect("cwd");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");

    let faux = create_faux_core(RegisterFauxProviderOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            reasoning: Some(true),
            ..Default::default()
        }]),
        ..Default::default()
    });
    let provider_id = faux.provider().to_string();
    let api = faux.api().to_string();
    let models = faux.models().to_vec();
    kiana_ai::compat::register_api_provider(
        kiana_ai::compat::ApiProvider::from_streams(&api, Arc::new(faux.clone())),
        None,
    );
    let auth_storage = AuthStorage::in_memory(auth_data_from_value(json!({
        provider_id.clone(): { "type": "api_key", "key": "faux-key" }
    })));
    let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(auth_storage)),
        models_path: ModelsPathOption::Disabled,
        allow_model_network: false,
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
    .expect("model runtime");
    let provider = create_provider(CreateProviderOptions {
        id: provider_id.clone(),
        name: Some(provider_id.clone()),
        base_url: Some(models[0].base_url.clone()),
        headers: None,
        auth: kiana_ai::auth::types::ProviderAuth::default(),
        models: models
            .iter()
            .cloned()
            .map(kiana_ai::types::AnyModel::Chat)
            .collect(),
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(ProviderApiImpl::Single(Arc::new(faux.clone()))),
        images: None,
        classifiers: None,
    })
    .expect("faux provider");
    let _ = model_runtime.register_native_provider(provider);
    let _ = model_runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        })
        .await;

    let settings_manager = SettingsManager::in_memory(Settings::default());
    let resources_for_factory = resources.clone();
    let agent_dir_for_factory = agent_dir.clone();
    let settings_for_factory = settings_manager.clone();
    let model_runtime_for_factory = model_runtime.clone();
    let model_for_factory = models[0].clone();
    let factory: CreateAgentSessionRuntimeFactory = Arc::new(move |input| {
        let resources = resources_for_factory.clone();
        let agent_dir = agent_dir_for_factory.clone();
        let settings_manager = settings_for_factory.clone();
        let model_runtime = model_runtime_for_factory.clone();
        let model = model_for_factory.clone();
        Box::pin(async move {
            let skills = resources.skills.clone();
            let skill_diagnostics = resources.skill_diagnostics.clone();
            let prompts = resources.prompts.clone();
            let prompt_diagnostics = resources.prompt_diagnostics.clone();
            let themes = resources.themes.clone();
            let theme_diagnostics = resources.theme_diagnostics.clone();
            let agents_files = resources.agents_files.clone();
            let loader_options = DefaultResourceLoaderOptions {
                cwd: input.cwd.clone(),
                agent_dir: agent_dir.clone(),
                settings_manager: Some(settings_manager.clone()),
                no_context_files: true,
                system_prompt: resources.system_prompt_path.clone(),
                append_system_prompt: if resources.append_system_prompt_paths.is_empty() {
                    None
                } else {
                    Some(resources.append_system_prompt_paths.clone())
                },
                skills_override: Some(Box::new(move |mut result: SkillsResult| {
                    result.skills = skills.clone();
                    result.diagnostics = skill_diagnostics.clone();
                    result
                })),
                prompts_override: Some(Box::new(move |mut result: PromptsResult| {
                    result.prompts = prompts.clone();
                    result.diagnostics = prompt_diagnostics.clone();
                    result
                })),
                themes_override: Some(Box::new(move |mut result: ThemesResult| {
                    result.themes = themes.clone();
                    result.diagnostics = theme_diagnostics.clone();
                    result
                })),
                agents_files_override: Some(Box::new(move |_result: AgentsFilesResult| {
                    AgentsFilesResult {
                        agents_files: agents_files.clone(),
                    }
                })),
                ..Default::default()
            };
            let services = create_agent_session_services(CreateAgentSessionServicesOptions {
                cwd: input.cwd.clone(),
                agent_dir: Some(agent_dir),
                settings_manager: Some(settings_manager),
                model_runtime: Some(model_runtime.clone()),
                resource_loader_options: Some(loader_options),
                ..Default::default()
            })
            .await
            .map_err(|error| AgentSessionRuntimeError::Other(error.message))?;
            let created =
                create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
                    services: services.clone(),
                    session_manager: input.session_manager,
                    session_start_event: input.session_start_event,
                    model: Some(model),
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
                .map_err(|error| AgentSessionRuntimeError::Other(error.to_string()))?;
            Ok(CreateAgentSessionRuntimeResult {
                session: created.session,
                extensions_result: created.extensions_result,
                model_fallback_message: created.model_fallback_message,
                diagnostics: services.diagnostics.clone(),
                services,
            })
        })
    });

    let session_manager = SessionManager::in_memory(&cwd, None, None).expect("session manager");
    let runtime = create_agent_session_runtime(
        factory,
        kiana_session::agent_session_runtime::CreateAgentSessionRuntimeOptions {
            cwd: cwd.clone(),
            agent_dir: agent_dir.clone(),
            session_manager,
            session_start_event: None,
        },
    )
    .await
    .expect("runtime");
    let mode = InteractiveMode::new(
        runtime,
        InteractiveModeOptions {
            terminal: Some(Box::new(common::TestTerminal::new())),
            tui_mode: Some(TuiMode::Regular),
            ..Default::default()
        },
    );
    Fixture {
        mode,
        _temp: temp_dir,
        _engine: engine,
        _faux: faux,
        cwd,
    }
}

fn source(source: &str, scope: SourceScope, base_dir: Option<&str>) -> SourceInfo {
    SourceInfo {
        path: String::new(),
        source: source.to_string(),
        scope,
        origin: SourceOrigin::TopLevel,
        base_dir: base_dir.map(str::to_string),
    }
}

fn skill(name: &str, path: &str, source_info: SourceInfo) -> Skill {
    Skill {
        name: name.to_string(),
        description: String::new(),
        file_path: path.to_string(),
        base_dir: path
            .rsplit_once('/')
            .map(|(dir, _)| dir.to_string())
            .unwrap_or_default(),
        source_info,
        disable_model_invocation: false,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn quiet_startup_hides_both_listing_and_diagnostics() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture(Resources::default()).await;
        fixture
            .mode
            .settings_manager()
            .set_quiet_startup(QuietStartup::Boolean(true));
        fixture.mode.test_show_loaded_resources(false, false);
        assert!(fixture.mode.test_loaded_resources_text().trim().is_empty());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn quiet_startup_keeps_diagnostics_when_requested() {
    let _theme = theme();
    common::run_local(async {
        let resources = Resources {
            skill_diagnostics: vec![ResourceDiagnostic {
                kind: DiagnosticKind::Warning,
                message: "duplicate skill".to_string(),
                path: None,
                collision: None,
            }],
            ..Default::default()
        };
        let mut fixture = fixture(resources).await;
        fixture
            .mode
            .settings_manager()
            .set_quiet_startup(QuietStartup::Boolean(true));
        fixture.mode.test_show_loaded_resources(false, true);
        let text = fixture.mode.test_loaded_resources_text();
        assert!(text.contains("[Skill conflicts]"), "{text}");
        assert!(text.contains("duplicate skill"), "{text}");
        assert!(!text.contains("[Skills]"), "{text}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn compact_listing_shows_sorted_skill_names_and_sections() {
    let _theme = theme();
    common::run_local(async {
        let resources = Resources {
            skills: vec![
                skill(
                    "zebra",
                    "/project/.kiana/skills/zebra/SKILL.md",
                    source("local", SourceScope::Project, None),
                ),
                skill(
                    "alpha",
                    "/home/user/.kiana/skills/alpha/SKILL.md",
                    source("local", SourceScope::User, None),
                ),
            ],
            ..Default::default()
        };
        let mut fixture = fixture(resources).await;
        fixture
            .mode
            .settings_manager()
            .set_quiet_startup(QuietStartup::Boolean(false));
        fixture.mode.test_show_loaded_resources(false, false);
        let text = fixture.mode.test_loaded_resources_text();
        assert!(text.contains("[Skills]"), "{text}");
        assert!(text.contains("alpha, zebra"), "{text}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn verbose_startup_expands_sections() {
    let _theme = theme();
    common::run_local(async {
        let resources = Resources {
            skills: vec![skill(
                "alpha",
                "/project/.kiana/skills/alpha/SKILL.md",
                source("local", SourceScope::Project, None),
            )],
            ..Default::default()
        };
        let mut fixture = fixture(resources).await;
        fixture
            .mode
            .settings_manager()
            .set_quiet_startup(QuietStartup::Boolean(false));
        fixture.mode.test_set_verbose(true);
        fixture.mode.test_show_loaded_resources(false, false);
        let text = fixture.mode.test_loaded_resources_text();
        // 展开态：scope 与文件路径。
        assert!(text.contains("project"), "{text}");
        assert!(text.contains("SKILL.md"), "{text}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn context_files_render_in_loader_order_with_relative_paths() {
    let _theme = theme();
    common::run_local(async {
        let temp = tempfile::tempdir().expect("temp");
        let system_prompt = temp.path().join("SYSTEM.md");
        let append = temp.path().join("APPEND.md");
        std::fs::write(&system_prompt, "system").expect("system prompt");
        std::fs::write(&append, "append").expect("append prompt");
        let agents_file = temp.path().join("AGENTS.md");
        std::fs::write(&agents_file, "agents").expect("agents");
        let resources = Resources {
            system_prompt_path: Some(system_prompt.to_string_lossy().into_owned()),
            append_system_prompt_paths: vec![append.to_string_lossy().into_owned()],
            agents_files: vec![ContextFile {
                path: agents_file.to_string_lossy().into_owned(),
                content: "agents".to_string(),
            }],
            ..Default::default()
        };
        let mut fixture = fixture(resources).await;
        fixture
            .mode
            .settings_manager()
            .set_quiet_startup(QuietStartup::Boolean(false));
        fixture.mode.test_show_loaded_resources(false, false);
        let text = fixture.mode.test_loaded_resources_text();
        assert!(text.contains("[Context]"), "{text}");
        let system_index = text.find("SYSTEM.md").expect("system prompt");
        let append_index = text.find("APPEND.md").expect("append prompt");
        let agents_index = text.find("AGENTS.md").expect("agents file");
        assert!(system_index < append_index, "{text}");
        assert!(append_index < agents_index, "{text}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn extension_labels_collapse_index_and_package_sources() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture(Resources::default()).await;
        let labels = fixture.mode.test_compact_extension_labels(&[
            (
                "/repo/node_modules/pkg-a/extensions/index.ts".to_string(),
                Some(source(
                    "npm:pkg-a",
                    SourceScope::User,
                    Some("/repo/node_modules/pkg-a"),
                )),
            ),
            (
                "/repo/node_modules/pkg-a/extensions/sibling/index.ts".to_string(),
                Some(source(
                    "npm:pkg-a",
                    SourceScope::User,
                    Some("/repo/node_modules/pkg-a"),
                )),
            ),
            (
                "/repo/.kiana/extensions/tools/index.ts".to_string(),
                Some(source("local", SourceScope::Project, None)),
            ),
        ]);
        assert_eq!(labels[0], "pkg-a");
        assert_eq!(labels[1], "pkg-a:sibling");
        assert_eq!(labels[2], "tools");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn collision_diagnostics_group_winner_and_losers() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture(Resources::default()).await;
        let diagnostics = vec![
            ResourceDiagnostic {
                kind: DiagnosticKind::Collision,
                message: String::new(),
                path: None,
                collision: Some(ResourceCollision {
                    resource_type: ResourceType::Skill,
                    name: "review".to_string(),
                    winner_path: "/project/.kiana/skills/review".to_string(),
                    loser_path: "/home/user/.kiana/skills/review".to_string(),
                    winner_source: None,
                    loser_source: None,
                }),
            },
            ResourceDiagnostic {
                kind: DiagnosticKind::Error,
                message: "boom".to_string(),
                path: Some("/project/.kiana/prompts/a.md".to_string()),
                collision: None,
            },
        ];
        let text = fixture
            .mode
            .test_format_diagnostics(&diagnostics, &indexmap::IndexMap::new());
        assert!(text.contains("\"review\" collision:"), "{text}");
        assert!(text.contains("✓"), "{text}");
        assert!(text.contains("✗"), "{text}");
        assert!(text.contains("(skipped)"), "{text}");
        assert!(text.contains("boom"), "{text}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn path_labels_strip_home_and_windows_separators() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture(Resources::default()).await;
        let labels = fixture.mode.test_compact_extension_labels(&[(
            "C:\\Users\\me\\.kiana\\extensions\\tools\\index.ts".to_string(),
            Some(source("local", SourceScope::User, None)),
        )]);
        assert_eq!(labels[0], "tools");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn cwd_relative_context_paths_are_used_for_compact_listing() {
    let _theme = theme();
    common::run_local(async {
        let resources = Resources {
            agents_files: vec![ContextFile {
                path: "AGENTS.md".to_string(),
                content: String::new(),
            }],
            ..Default::default()
        };
        let mut fixture = fixture(resources).await;
        fixture.mode.test_show_loaded_resources(false, false);
        let text = normalize_listing(&fixture.mode.test_loaded_resources_text());
        assert!(text.contains("[Context]"), "{text}");
        assert!(text.contains("AGENTS.md"), "{text}");
        assert!(
            !text.contains(&fixture.cwd),
            "cwd-relative path must not include the absolute cwd: {text}"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// `interactive-mode-status.test.ts` 的其余 showLoadedResources 用例
// ---------------------------------------------------------------------------

/// 去掉 ANSI 后的渲染文本；行尾空白/空行归一化（快照比较用）。
fn normalize_listing(text: &str) -> String {
    text.replace('\\', "/")
        .lines()
        .map(|line| {
            kiana_tui::text::strip_terminal_sequences(line)
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn extension_item(
    path: &str,
    source_name: &str,
    scope: SourceScope,
    base_dir: Option<&str>,
) -> (String, SourceInfo) {
    (path.to_string(), source(source_name, scope, base_dir))
}

fn mixed_extension_fixtures() -> Vec<(String, SourceInfo)> {
    vec![
        extension_item(
            "/tmp/project/.kiana/extensions/answer.ts",
            "local",
            SourceScope::Project,
            Some("/tmp/project/.kiana/extensions"),
        ),
        extension_item(
            "/tmp/project/.kiana/extensions/local-index/index.ts",
            "local",
            SourceScope::Project,
            Some("/tmp/project/.kiana/extensions"),
        ),
        extension_item(
            "/tmp/agent/extensions/user-index/index.ts",
            "local",
            SourceScope::User,
            Some("/tmp/agent/extensions"),
        ),
        extension_item(
            "/tmp/project/.kiana/npm/node_modules/kiana-markdown-preview/extensions/index.ts",
            "npm:kiana-markdown-preview",
            SourceScope::Project,
            Some("/tmp/project/.kiana/npm/node_modules/kiana-markdown-preview"),
        ),
        extension_item(
            "/tmp/project/.kiana/npm/node_modules/@scope/kiana-scoped/extensions/index.ts",
            "npm:@scope/kiana-scoped",
            SourceScope::Project,
            Some("/tmp/project/.kiana/npm/node_modules/@scope/kiana-scoped"),
        ),
        extension_item(
            "/tmp/project/.kiana/git/github.com/HazAT/kiana-interactive-subagents/extensions/index.ts",
            "git:github.com/HazAT/kiana-interactive-subagents",
            SourceScope::Project,
            Some("/tmp/project/.kiana/git/github.com/HazAT/kiana-interactive-subagents"),
        ),
        extension_item(
            "/tmp/project/.kiana/git/github.com/HazAT/kiana-interactive-subagents/extensions/subagents/index.ts",
            "git:github.com/HazAT/kiana-interactive-subagents",
            SourceScope::Project,
            Some("/tmp/project/.kiana/git/github.com/HazAT/kiana-interactive-subagents"),
        ),
        extension_item(
            "/tmp/temp/cli-extension.ts",
            "cli",
            SourceScope::Temporary,
            Some("/tmp/temp"),
        ),
    ]
}

#[tokio::test(flavor = "current_thread")]
async fn compact_extension_listing_matches_the_ts_snapshot() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture(Resources::default()).await;
        let items: Vec<(String, Option<SourceInfo>)> =
            mixed_extension_fixtures().into_iter().map(|(path, info)| (path, Some(info))).collect();
        let text = normalize_listing(&format!(
            "[Extensions]\n{}",
            fixture.mode.test_render_extensions_listing(&items, false)
        ));
        assert_eq!(
            text,
            "[Extensions]\n  @scope/kiana-scoped, answer.ts, cli-extension.ts, HazAT/kiana-interactive-subagents, HazAT/kiana-interactive-subagents:subagents, kiana-markdown-preview, local-index, user-index"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn expanded_extension_listing_matches_the_ts_snapshot() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture(Resources::default()).await;
        let items: Vec<(String, Option<SourceInfo>)> =
            mixed_extension_fixtures().into_iter().map(|(path, info)| (path, Some(info))).collect();
        let text = normalize_listing(&format!(
            "[Extensions]\n{}",
            fixture.mode.test_render_extensions_listing(&items, true)
        ));
        assert_eq!(
            text,
            "[Extensions]\n  project\n    /tmp/project/.kiana/extensions/answer.ts\n    /tmp/project/.kiana/extensions/local-index\n    git:github.com/HazAT/kiana-interactive-subagents\n      extensions\n      extensions/subagents\n    npm:@scope/kiana-scoped\n      extensions\n    npm:kiana-markdown-preview\n      extensions\n  user\n    /tmp/agent/extensions/user-index\n  path\n    /tmp/temp/cli-extension.ts"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn full_listing_is_shown_when_expanded_and_on_verbose_startup() {
    let _theme = theme();
    common::run_local(async {
        let resources = Resources {
            skills: vec![skill(
                "commit",
                "/tmp/skill/SKILL.md",
                source("local", SourceScope::User, None),
            )],
            ..Default::default()
        };
        let mut fixture = fixture(resources).await;
        fixture.mode.test_show_loaded_resources(false, false);
        let compact = normalize_listing(&fixture.mode.test_loaded_resources_text());
        assert!(compact.contains("[Skills]"), "{compact}");
        assert!(compact.contains("commit"), "{compact}");
        assert!(!compact.contains("/tmp/skill/SKILL.md"), "{compact}");

        // 展开态（tool output expanded / verbose）显示路径而不是名字。
        fixture.mode.test_set_tools_expanded(true);
        fixture.mode.test_show_loaded_resources(false, false);
        let expanded = normalize_listing(&fixture.mode.test_loaded_resources_text());
        assert!(expanded.contains("/tmp/skill/SKILL.md"), "{expanded}");
        assert!(!expanded.contains("commit"), "{expanded}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn verbose_startup_expands_while_tool_output_stays_collapsed() {
    let _theme = theme();
    common::run_local(async {
        let resources = Resources {
            skills: vec![skill(
                "commit",
                "/tmp/skill/SKILL.md",
                source("local", SourceScope::User, None),
            )],
            ..Default::default()
        };
        let mut fixture = fixture(resources).await;
        fixture
            .mode
            .settings_manager()
            .set_quiet_startup(QuietStartup::Boolean(true));
        fixture.mode.test_set_verbose(true);
        fixture.mode.test_show_loaded_resources(false, false);
        let text = normalize_listing(&fixture.mode.test_loaded_resources_text());
        assert!(text.contains("[Skills]"), "{text}");
        assert!(text.contains("/tmp/skill/SKILL.md"), "{text}");
        assert!(!text.contains("commit"), "{text}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn compact_extension_labels_abbreviate_and_disambiguate() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture(Resources::default()).await;

        // 单文件扩展只留文件名。
        let labels = fixture.mode.test_compact_extension_labels(&[
            ("/tmp/extensions/answer.ts".to_string(), None),
            ("/tmp/extensions/btw.ts".to_string(), None),
        ]);
        assert_eq!(labels, vec!["answer.ts", "btw.ts"]);

        // 同名末级目录：逐级加上父目录。
        let labels = fixture.mode.test_compact_extension_labels(&[
            (
                "/tmp/alpha/one/index.ts".to_string(),
                Some(source("cli", SourceScope::Temporary, Some("/tmp/alpha"))),
            ),
            (
                "/tmp/beta/one/index.ts".to_string(),
                Some(source("cli", SourceScope::Temporary, Some("/tmp/beta"))),
            ),
            (
                "/tmp/gamma/one/index.ts".to_string(),
                Some(source("cli", SourceScope::Temporary, Some("/tmp/gamma"))),
            ),
        ]);
        assert_eq!(labels, vec!["alpha/one", "beta/one", "gamma/one"]);

        // index.ts / index.js 去掉后显示父目录。
        for file in ["index.ts", "index.js"] {
            let path = format!("/tmp/extensions/plan-mode/{file}");
            let labels = fixture.mode.test_compact_extension_labels(&[(
                path.clone(),
                Some(source(
                    "local",
                    SourceScope::Project,
                    Some("/tmp/extensions"),
                )),
            )]);
            assert_eq!(labels, vec!["plan-mode"], "{file}");
        }

        // 单文件与 index.ts 混合。
        let labels = fixture.mode.test_compact_extension_labels(&[
            (
                "/tmp/extensions/webfetch.ts".to_string(),
                Some(source(
                    "local",
                    SourceScope::Project,
                    Some("/tmp/extensions"),
                )),
            ),
            (
                "/tmp/extensions/plan-mode/index.ts".to_string(),
                Some(source(
                    "local",
                    SourceScope::Project,
                    Some("/tmp/extensions"),
                )),
            ),
        ]);
        assert_eq!(labels, vec!["webfetch.ts", "plan-mode"]);

        // 父目录不同：无需消歧。
        let labels = fixture.mode.test_compact_extension_labels(&[
            (
                "/tmp/extensions/foo/index.ts".to_string(),
                Some(source(
                    "local",
                    SourceScope::Project,
                    Some("/tmp/extensions"),
                )),
            ),
            (
                "/tmp/extensions/bar/index.ts".to_string(),
                Some(source(
                    "local",
                    SourceScope::Project,
                    Some("/tmp/extensions"),
                )),
            ),
        ]);
        assert_eq!(labels, vec!["foo", "bar"]);

        // 同名父目录：用祖父目录消歧。
        let labels = fixture.mode.test_compact_extension_labels(&[
            (
                "/tmp/alpha/tools/index.ts".to_string(),
                Some(source("cli", SourceScope::Temporary, Some("/tmp/alpha"))),
            ),
            (
                "/tmp/beta/tools/index.ts".to_string(),
                Some(source("cli", SourceScope::Temporary, Some("/tmp/beta"))),
            ),
        ]);
        assert_eq!(labels, vec!["alpha/tools", "beta/tools"]);

        // 非 index 文件保持文件名。
        let labels = fixture.mode.test_compact_extension_labels(&[(
            "/tmp/extensions/my-ext/main.ts".to_string(),
            Some(source(
                "local",
                SourceScope::Project,
                Some("/tmp/extensions"),
            )),
        )]);
        assert_eq!(labels, vec!["main.ts"]);

        // 包扩展仍然去掉 index.ts。
        let labels = fixture.mode.test_compact_extension_labels(&[(
            "/tmp/project/.kiana/npm/node_modules/kiana-markdown-preview/extensions/index.ts"
                .to_string(),
            Some(source(
                "npm:kiana-markdown-preview",
                SourceScope::Project,
                Some("/tmp/project/.kiana/npm/node_modules/kiana-markdown-preview"),
            )),
        )]);
        assert_eq!(labels, vec!["kiana-markdown-preview"]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn npm_sibling_extension_labels_are_relative_to_the_declaring_package() {
    let _theme = theme();
    common::run_local(async {
        let fixture = fixture(Resources::default()).await;
        let base = "/tmp/project/.kiana/npm/node_modules/primary-package";
        let labels = fixture.mode.test_compact_extension_labels(&[
            (
                "/tmp/project/.kiana/npm/node_modules/primary-package/index.ts".to_string(),
                Some(source(
                    "npm:primary-package",
                    SourceScope::Project,
                    Some(base),
                )),
            ),
            (
                "/tmp/project/.kiana/npm/node_modules/sibling-package/index.ts".to_string(),
                Some(source(
                    "npm:primary-package",
                    SourceScope::Project,
                    Some(base),
                )),
            ),
        ]);
        assert_eq!(
            labels,
            vec!["primary-package", "primary-package:../sibling-package"]
        );

        // Windows 分隔符。
        let base = "C:\\Users\\me\\.kiana\\agent\\npm\\node_modules\\primary-package";
        let labels = fixture.mode.test_compact_extension_labels(&[
            (
                "C:\\Users\\me\\.kiana\\agent\\npm\\node_modules\\primary-package\\index.ts"
                    .to_string(),
                Some(source("npm:primary-package", SourceScope::User, Some(base))),
            ),
            (
                "C:\\Users\\me\\.kiana\\agent\\npm\\node_modules\\sibling-package\\index.ts"
                    .to_string(),
                Some(source("npm:primary-package", SourceScope::User, Some(base))),
            ),
        ]);
        assert_eq!(
            labels,
            vec!["primary-package", "primary-package:../sibling-package"]
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn context_paths_use_tilde_for_home_and_relative_for_cwd() {
    let _theme = theme();
    common::run_local(async {
        let home = std::env::var("HOME").expect("HOME");
        let resources = Resources {
            agents_files: vec![
                ContextFile {
                    path: format!("{home}/.kiana/agent/AGENTS.md"),
                    content: String::new(),
                },
                ContextFile {
                    path: "AGENTS.md".to_string(),
                    content: String::new(),
                },
            ],
            ..Default::default()
        };
        let mut fixture = fixture(resources).await;
        fixture.mode.test_show_loaded_resources(false, false);
        let text = normalize_listing(&fixture.mode.test_loaded_resources_text());
        assert!(text.contains("[Context]"), "{text}");
        assert!(
            text.contains("~/.kiana/agent/AGENTS.md, AGENTS.md"),
            "{text}"
        );
        assert!(
            !text.contains(&format!("{home}/.kiana/agent/AGENTS.md")),
            "{text}"
        );

        // 展开态显示完整（home 缩短为 ~ 的）路径。
        assert_eq!(
            fixture
                .mode
                .test_format_display_path(&format!("{home}/Development/kiana-mono/AGENTS.md")),
            "~/Development/kiana-mono/AGENTS.md"
        );
        fixture.mode.test_set_verbose(true);
        fixture.mode.test_show_loaded_resources(false, false);
        let expanded = normalize_listing(&fixture.mode.test_loaded_resources_text());
        assert!(expanded.contains("~/.kiana/agent/AGENTS.md"), "{expanded}");
        assert!(
            !expanded.contains("~/.kiana/agent/AGENTS.md, AGENTS.md"),
            "{expanded}"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn quiet_startup_header_and_details_follow_the_setting() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture(Resources::default()).await;

        fixture
            .mode
            .settings_manager()
            .set_quiet_startup(QuietStartup::Header);
        assert!(fixture.mode.test_should_show_startup_header());
        assert!(!fixture.mode.test_should_show_startup_details());

        fixture
            .mode
            .settings_manager()
            .set_quiet_startup(QuietStartup::Boolean(true));
        assert!(!fixture.mode.test_should_show_startup_header());
        assert!(!fixture.mode.test_should_show_startup_details());

        fixture.mode.test_set_verbose(true);
        assert!(fixture.mode.test_should_show_startup_header());
        assert!(fixture.mode.test_should_show_startup_details());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn quiet_startup_reload_keeps_diagnostics_but_hides_the_listing() {
    let _theme = theme();
    common::run_local(async {
        let resources = Resources {
            skills: vec![skill(
                "commit",
                "/tmp/skill/SKILL.md",
                source("local", SourceScope::User, None),
            )],
            skill_diagnostics: vec![ResourceDiagnostic {
                kind: DiagnosticKind::Warning,
                message: "duplicate skill name".to_string(),
                path: None,
                collision: None,
            }],
            ..Default::default()
        };
        let mut fixture = fixture(resources).await;
        fixture
            .mode
            .settings_manager()
            .set_quiet_startup(QuietStartup::Boolean(true));
        fixture.mode.test_show_loaded_resources(false, true);
        let text = normalize_listing(&fixture.mode.test_loaded_resources_text());
        assert!(text.contains("[Skill conflicts]"), "{text}");
        assert!(!text.contains("[Skills]"), "{text}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn show_loaded_resources_clears_stale_entries_before_rendering() {
    let _theme = theme();
    common::run_local(async {
        let resources = Resources {
            skills: vec![skill(
                "commit",
                "/tmp/skill/SKILL.md",
                source("local", SourceScope::User, None),
            )],
            ..Default::default()
        };
        let mut fixture = fixture(resources).await;
        fixture.mode.test_push_loaded_resources_child(Box::new(
            kiana_tui::components::text_component::Text::with_padding("stale resources", 0, 0),
        ));
        fixture.mode.test_show_loaded_resources(false, false);
        let text = normalize_listing(&fixture.mode.test_loaded_resources_text());
        assert!(!text.contains("stale resources"), "{text}");
        assert!(text.contains("[Skills]"), "{text}");
    })
    .await;
}

/// 展开状态同时作用于 loadedResourcesContainer 里的 `ExpandableText` 区块（TS
/// `setToolsExpanded` 的 loadedResources 分支）。
#[tokio::test(flavor = "current_thread")]
async fn set_tools_expanded_updates_loaded_resource_sections() {
    let _theme = theme();
    common::run_local(async {
        let resources = Resources {
            skills: vec![skill(
                "commit",
                "/tmp/skill/SKILL.md",
                source("local", SourceScope::User, None),
            )],
            ..Default::default()
        };
        let mut fixture = fixture(resources).await;
        fixture.mode.test_show_loaded_resources(false, false);
        let collapsed = normalize_listing(&fixture.mode.test_loaded_resources_text());
        assert!(collapsed.contains("commit"), "{collapsed}");
        assert!(!collapsed.contains("/tmp/skill/SKILL.md"), "{collapsed}");

        fixture.mode.test_set_tools_expanded(true);
        let expanded = normalize_listing(&fixture.mode.test_loaded_resources_text());
        assert!(expanded.contains("/tmp/skill/SKILL.md"), "{expanded}");
        assert!(!expanded.contains("commit"), "{expanded}");
    })
    .await;
}
