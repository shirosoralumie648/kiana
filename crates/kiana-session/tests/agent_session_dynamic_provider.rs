//! `test/agent-session-dynamic-provider.test.ts`：扩展 provider 注册对活动模型生效。
//!
//! Rust 侧没有 `session.agent.streamFunction` 的公开替换点；这里用捕获型 API 实现观察
//! 真正发往 provider 的 `model.baseUrl`（等价断言）。
#![allow(clippy::arc_with_non_send_sync)]

mod session_support;

use std::sync::{Arc, Mutex};

use kiana_ai::auth::types::ProviderAuth;
use kiana_ai::event_stream::create_assistant_message_event_stream;
use kiana_ai::models::provider::{Provider, ProviderStreams};
use kiana_ai::providers::faux::faux_assistant_message;
use kiana_ai::types::{Model, SimpleStreamOptions, StopReason, StreamOptions, TranscriptContext};
use kiana_config::auth::storage::{AuthStorage, auth_data_from_value};
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use kiana_ext_api::events::{ExtensionEventKind, ExtensionHandler};
use kiana_ext_api::types::{
    ExtensionFactory, InlineExtension, ProviderConfig, ProviderRegistration, RegisterCommandOptions,
};
use kiana_resources::loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader,
};
use kiana_session::agent_session::PromptOptions;
use kiana_session::sdk::{CreateAgentSessionOptions, create_agent_session};
use kiana_session_core::session_manager::SessionManager;
use serde_json::json;

use session_support::{install_engine, lock, run};

thread_local! {
    static KIANA_SLOT: std::cell::RefCell<Option<Arc<dyn kiana_ext_api::types::ExtensionApi>>> =
        const { std::cell::RefCell::new(None) };
    static CAPTURE: std::cell::RefCell<Option<Arc<CaptureApi>>> =
        const { std::cell::RefCell::new(None) };
}

#[derive(Default)]
struct CaptureApi {
    base_urls: Mutex<Vec<String>>,
}

impl ProviderStreams for CaptureApi {
    fn stream(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: StreamOptions,
    ) -> kiana_ai::event_stream::AssistantMessageEventStream {
        lock(&self.base_urls).push(model.base_url.clone());
        let stream = create_assistant_message_event_stream();
        let mut message = faux_assistant_message("ok", Default::default());
        message.provider = model.provider.clone();
        message.model = model.id.clone();
        message.stop_reason = StopReason::Stop;
        stream.end(message);
        stream
    }
    fn stream_simple(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: SimpleStreamOptions,
    ) -> kiana_ai::event_stream::AssistantMessageEventStream {
        self.stream(
            model,
            &TranscriptContext::default(),
            StreamOptions::default(),
        )
    }
}

/// 捕获真正发往 provider 的 `model.baseUrl`，并结束一条 faux assistant 消息。
fn capture_stream(model: &Model) -> kiana_ai::event_stream::AssistantMessageEventStream {
    CAPTURE.with(|capture| {
        if let Some(capture) = capture.borrow().as_ref() {
            lock(&capture.base_urls).push(model.base_url.clone());
        }
    });
    let stream = create_assistant_message_event_stream();
    let mut message = faux_assistant_message("ok", Default::default());
    message.provider = model.provider.clone();
    message.model = model.id.clone();
    message.stop_reason = StopReason::Stop;
    stream.end(message);
    stream
}

/// TS 的 `auth: { apiKey: { resolve: async () => test-key } }`。
struct TestApiKeyAuth;

#[async_trait::async_trait]
impl kiana_ai::auth::types::ApiKeyAuth for TestApiKeyAuth {
    fn name(&self) -> &str {
        "Test API key"
    }

    async fn resolve(
        &self,
        _input: kiana_ai::auth::types::ApiKeyAuthInput<'_>,
    ) -> Result<Option<kiana_ai::auth::types::AuthResult>, kiana_ai::auth::types::AuthError> {
        Ok(Some(kiana_ai::auth::types::AuthResult::new(
            kiana_ai::auth::types::ModelAuth {
                api_key: Some("test-key".to_string()),
                ..Default::default()
            },
        )))
    }
}

/// TS 的 `nativeAnthropicProvider(baseUrl)`：原生 provider 对象（含 baseUrl 覆盖）。
struct NativeAnthropicProvider {
    model: Model,
    base_url: String,
    auth: ProviderAuth,
}

impl Provider for NativeAnthropicProvider {
    fn id(&self) -> &str {
        "anthropic"
    }

    fn name(&self) -> &str {
        "Native Anthropic"
    }

    fn base_url(&self) -> Option<&str> {
        Some(&self.base_url)
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        vec![self.model.clone()]
    }

    fn stream(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: StreamOptions,
    ) -> kiana_ai::event_stream::AssistantMessageEventStream {
        capture_stream(model)
    }

    fn stream_simple(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: SimpleStreamOptions,
    ) -> kiana_ai::event_stream::AssistantMessageEventStream {
        capture_stream(model)
    }
}

fn native_anthropic_provider(base_url: &str) -> Arc<dyn Provider> {
    let mut model =
        kiana_ai::compat::get_model("anthropic", "claude-sonnet-4-5").expect("claude-sonnet-4-5");
    model.base_url = base_url.to_string();
    Arc::new(NativeAnthropicProvider {
        model,
        base_url: base_url.to_string(),
        auth: ProviderAuth {
            api_key: Some(Arc::new(TestApiKeyAuth)),
            oauth: None,
        },
    })
}

fn provider_config(base_url: &str) -> ProviderConfig {
    ProviderConfig {
        name: Some("anthropic".to_string()),
        base_url: Some(base_url.to_string()),
        api_key: Some("test-key".to_string()),
        // kiana-config（与 TS 的 `validateExtensionProvider` 相同）要求注册 `streamSimple`
        // 时必须同时给出 `api`；TS 测试里的 `{ baseUrl }` 配置不带 streamSimple，迁移到
        // Rust 的自定义流实现后必须显式声明 api。
        api: Some("anthropic-messages".to_string()),
        stream_simple: Some(Arc::new(|model, _context, _options| capture_stream(model))),
        images: None,
        classifiers: None,
        headers: None,
        auth_header: None,
        models: None,
        refresh_models: None,
        oauth: None,
    }
}

async fn create_session(
    factory: ExtensionFactory,
    cwd: &str,
    agent_dir: &str,
) -> (kiana_session::agent_session::AgentSession, Arc<CaptureApi>) {
    let capture = Arc::new(CaptureApi::default());
    CAPTURE.with(|slot| *slot.borrow_mut() = Some(Arc::clone(&capture)));
    let settings_manager = kiana_config::settings::SettingsManager::in_memory(Default::default());
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
    let auth_storage = AuthStorage::in_memory(auth_data_from_value(json!({
        "anthropic": { "type": "api_key", "key": "test-key" }
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
    let result = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.to_string()),
        agent_dir: Some(agent_dir.to_string()),
        model: kiana_ai::compat::get_model("anthropic", "claude-sonnet-4-5"),
        model_runtime: Some(model_runtime),
        settings_manager: Some(settings_manager),
        session_manager: Some(SessionManager::in_memory(cwd, None, None).expect("session")),
        resource_loader: Some(loader),
        ..Default::default()
    })
    .await
    .expect("create session");
    (result.session, capture)
}

fn temp_dirs() -> (tempfile::TempDir, String, String) {
    let temp = tempfile::tempdir().expect("temp");
    let cwd = temp.path().to_string_lossy().into_owned();
    let agent_dir = temp.path().join("agent").to_string_lossy().into_owned();
    std::fs::create_dir_all(&agent_dir).expect("agent");
    (temp, cwd, agent_dir)
}

async fn capture_prompt_base_url(
    session: &kiana_session::agent_session::AgentSession,
    capture: &CaptureApi,
) -> String {
    session
        .prompt("hello".to_string(), PromptOptions::default())
        .await
        .expect("prompt");
    lock(&capture.base_urls).last().cloned().expect("base url")
}

#[tokio::test]
async fn applies_top_level_register_provider_overrides_to_the_active_model() {
    run(async {
        let _engine = install_engine();
        let (_temp, cwd, agent_dir) = temp_dirs();
        let factory: ExtensionFactory = Arc::new(|kiana| {
            kiana.register_provider(ProviderRegistration::Config {
                name: "anthropic".to_string(),
                config: provider_config("http://localhost:8080/top-level"),
            });
            Box::pin(async {})
        });
        let (session, capture) = create_session(factory, &cwd, &agent_dir).await;
        assert_eq!(
            session.model().expect("model").base_url,
            "http://localhost:8080/top-level"
        );
        assert_eq!(
            capture_prompt_base_url(&session, &capture).await,
            "http://localhost:8080/top-level"
        );
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn applies_session_start_register_provider_overrides_to_the_active_model() {
    run(async {
        let _engine = install_engine();
        let (_temp, cwd, agent_dir) = temp_dirs();
        let factory: ExtensionFactory = Arc::new(|kiana| {
            let kiana_for_start = Arc::clone(&kiana);
            let _ = kiana.on(
                ExtensionEventKind::SessionStart,
                ExtensionHandler::Standard(Arc::new(move |_event, _ctx| {
                    let kiana = Arc::clone(&kiana_for_start);
                    Box::pin(async move {
                        kiana.register_provider(ProviderRegistration::Config {
                            name: "anthropic".to_string(),
                            config: provider_config("http://localhost:8080/session-start"),
                        });
                        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(None)
                    })
                })),
            );
            Box::pin(async {})
        });
        let (session, capture) = create_session(factory, &cwd, &agent_dir).await;
        session.bind_extensions(Default::default()).await;
        assert_eq!(
            session.model().expect("model").base_url,
            "http://localhost:8080/session-start"
        );
        assert_eq!(
            capture_prompt_base_url(&session, &capture).await,
            "http://localhost:8080/session-start"
        );
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn applies_command_time_register_provider_overrides_without_reload() {
    run(async {
        let _engine = install_engine();
        let (_temp, cwd, agent_dir) = temp_dirs();
        // 命令 handler 是 `Send + Sync`，不能捕获 `Arc<dyn ExtensionApi>`（trait 不带
        // Send/Sync）；用线程本地槽位在会话线程上取回同一个 API 对象。
        let factory: ExtensionFactory = Arc::new(|kiana| {
            KIANA_SLOT.with(|slot| *slot.borrow_mut() = Some(Arc::clone(&kiana)));
            kiana.register_command(
                "use-proxy",
                RegisterCommandOptions {
                    description: Some("Use proxy".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |_args, _ctx| {
                        Box::pin(async move {
                            KIANA_SLOT.with(|slot| {
                                if let Some(kiana) = slot.borrow().as_ref() {
                                    kiana.register_provider(ProviderRegistration::Config {
                                        name: "anthropic".to_string(),
                                        config: provider_config("http://localhost:8080/command"),
                                    });
                                }
                            });
                        })
                    }),
                },
            );
            Box::pin(async {})
        });
        let (session, capture) = create_session(factory, &cwd, &agent_dir).await;
        session.bind_extensions(Default::default()).await;
        session
            .prompt("/use-proxy".to_string(), PromptOptions::default())
            .await
            .expect("command");
        assert_eq!(
            session.model().expect("model").base_url,
            "http://localhost:8080/command"
        );
        assert_eq!(
            capture_prompt_base_url(&session, &capture).await,
            "http://localhost:8080/command"
        );
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn registers_native_kiana_ai_providers_during_extension_loading() {
    run(async {
        let _engine = install_engine();
        let (_temp, cwd, agent_dir) = temp_dirs();
        let factory: ExtensionFactory = Arc::new(|kiana| {
            kiana.register_provider(ProviderRegistration::Native(native_anthropic_provider(
                "http://localhost:8080/native-top-level",
            )));
            Box::pin(async {})
        });
        let (session, capture) = create_session(factory, &cwd, &agent_dir).await;
        assert_eq!(
            session.model().expect("model").base_url,
            "http://localhost:8080/native-top-level"
        );
        assert_eq!(
            capture_prompt_base_url(&session, &capture).await,
            "http://localhost:8080/native-top-level"
        );
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn registers_native_kiana_ai_providers_at_command_time() {
    run(async {
        let _engine = install_engine();
        let (_temp, cwd, agent_dir) = temp_dirs();
        let factory: ExtensionFactory = Arc::new(|kiana| {
            KIANA_SLOT.with(|slot| *slot.borrow_mut() = Some(Arc::clone(&kiana)));
            kiana.register_command(
                "use-native",
                RegisterCommandOptions {
                    description: Some("Use native provider".to_string()),
                    get_argument_completions: None,
                    handler: Arc::new(move |_args, _ctx| {
                        Box::pin(async move {
                            KIANA_SLOT.with(|slot| {
                                if let Some(kiana) = slot.borrow().as_ref() {
                                    kiana.register_provider(ProviderRegistration::Native(
                                        native_anthropic_provider(
                                            "http://localhost:8080/native-command",
                                        ),
                                    ));
                                }
                            });
                        })
                    }),
                },
            );
            Box::pin(async {})
        });
        let (session, capture) = create_session(factory, &cwd, &agent_dir).await;
        session.bind_extensions(Default::default()).await;
        session
            .prompt("/use-native".to_string(), PromptOptions::default())
            .await
            .expect("command");
        assert_eq!(
            session.model().expect("model").base_url,
            "http://localhost:8080/native-command"
        );
        assert_eq!(
            capture_prompt_base_url(&session, &capture).await,
            "http://localhost:8080/native-command"
        );
        session.dispose();
    })
    .await;
}

#[allow(dead_code)]
fn _unused(_: serde_json::Value) {
    let _ = json!({});
}
