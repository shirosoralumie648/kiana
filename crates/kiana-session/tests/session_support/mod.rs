//! sdk / runtime 测试的共享夹具（`sdk-*`、`agent-session-*`、`default-tools-setting`、
//! `agent-session-runtime` 与其回归测试共用）。
//!
//! 与 `tests/common/mod.rs`（agent-session 单元的夹具）互补：这里通过
//! `create_agent_session` / `create_agent_session_runtime` 装配，因此只暴露“从外部可观察”的
//! 接口（会话 + 捕获用的 provider / stream），不持有内部 `Agent`。
#![allow(clippy::arc_with_non_send_sync)]
#![allow(dead_code)]

use std::sync::{Arc, Mutex, MutexGuard};

use kiana_ai::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream};
use kiana_ai::models::ModelsRefreshOptions;
use kiana_ai::models::provider::{
    CreateProviderOptions, ProviderApiImpl, ProviderStreams, create_provider,
};
use kiana_ai::providers::faux::{
    FauxCore, FauxModelDefinition, RegisterFauxProviderOptions, create_faux_core,
    faux_assistant_message,
};
use kiana_ai::types::{
    AssistantMessage, Message, Model, ModelThinkingLevel, SimpleStreamOptions, StreamOptions,
    TranscriptContext, UserContentBlock,
};
use kiana_config::auth::storage::{AuthStorage, auth_data_from_value};
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use kiana_config::settings::SettingsManager;
use kiana_config::settings::schema::Settings;
use kiana_ext_api::types::ExtensionFactory;
use kiana_resources::loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader,
};
use kiana_session::agent_session::{AgentSession, AgentSessionEvent};
use kiana_session_core::session_manager::SessionManager;
use serde_json::json;

/// 安装扩展运行时引擎（`kiana-extensions` 的 thread-local 单例）。
///
/// 扩展派发（即使没有 handler 的 `runner.emit`）要求当前线程已安装引擎；返回值必须活到
/// 会话结束（`JsEngine` 是所有扩展回调的宿主）。
pub struct TestEngine {
    _engine: std::rc::Rc<kiana_extensions::engine::JsEngine>,
}

impl TestEngine {
    /// 引擎的 `Rc`（`kiana_extensions::loader::install` 需要它把引擎登记为当前引擎）。
    pub fn rc(&self) -> std::rc::Rc<kiana_extensions::engine::JsEngine> {
        std::rc::Rc::clone(&self._engine)
    }
}

pub fn install_engine() -> TestEngine {
    let engine = std::rc::Rc::new(kiana_extensions::engine::JsEngine::new().expect("js engine"));
    kiana_extensions::engine::install(&engine).expect("engine prelude");
    kiana_extensions::runner::context::install(&engine).expect("context.js");
    kiana_extensions::runner::dispatch::install(std::rc::Rc::clone(&engine)).expect("dispatch.js");
    TestEngine { _engine: engine }
}

/// 在 current_thread runtime + `LocalSet` 上运行测试体（扩展派发要求同线程）。
pub async fn run<F: std::future::Future>(future: F) -> F::Output {
    let local = tokio::task::LocalSet::new();
    local.run_until(future).await
}

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// faux provider 的 SDK 夹具。
pub struct SdkFixture {
    pub engine: TestEngine,
    pub temp_dir: tempfile::TempDir,
    pub cwd: String,
    pub agent_dir: String,
    pub faux: FauxCore,
    pub model_runtime: ModelRuntime,
    pub settings_manager: SettingsManager,
    pub auth_storage: AuthStorage,
    pub models: Vec<Model>,
}

impl SdkFixture {
    pub fn model(&self, id: Option<&str>) -> Option<Model> {
        self.faux.get_model(id).cloned()
    }

    pub fn set_responses(&self, responses: Vec<kiana_ai::providers::faux::FauxResponseStep>) {
        self.faux.set_responses(responses);
    }

    pub fn cleanup(&self) {}
}

/// 创建 faux provider + 已配置 auth 的 `ModelRuntime` 夹具。
pub async fn sdk_fixture(settings: Option<Settings>) -> SdkFixture {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let cwd = temp_dir.path().join("project");
    let agent_dir = temp_dir.path().join("agent");
    std::fs::create_dir_all(&cwd).expect("cwd");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    let cwd = cwd.to_string_lossy().into_owned();
    let agent_dir = agent_dir.to_string_lossy().into_owned();

    let engine = install_engine();
    let faux = create_faux_core(RegisterFauxProviderOptions {
        models: Some(vec![
            FauxModelDefinition {
                id: "faux-1".to_string(),
                reasoning: Some(true),
                ..Default::default()
            },
            FauxModelDefinition {
                id: "faux-2".to_string(),
                reasoning: Some(false),
                ..Default::default()
            },
        ]),
        ..Default::default()
    });
    let provider_id = faux.provider().to_string();
    let api = faux.api().to_string();
    let models: Vec<Model> = faux.models().to_vec();
    kiana_ai::compat::register_api_provider(
        kiana_ai::compat::ApiProvider::from_streams(&api, Arc::new(faux.clone())),
        None,
    );

    let auth_storage = AuthStorage::in_memory(auth_data_from_value(json!({
        provider_id.clone(): { "type": "api_key", "key": "faux-key" }
    })));
    let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(auth_storage.clone())),
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
    for _ in 0..200 {
        let _ = model_runtime
            .refresh(ModelsRefreshOptions {
                allow_network: Some(false),
                ..Default::default()
            })
            .await;
        if model_runtime.has_configured_auth(&provider_id) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }

    let settings_manager = SettingsManager::in_memory(settings.unwrap_or_default());
    SdkFixture {
        engine,
        temp_dir,
        cwd,
        agent_dir,
        faux,
        model_runtime,
        settings_manager,
        auth_storage,
        models,
    }
}

/// 创建一个用给定扩展工厂 reload 过的 `DefaultResourceLoader`。
pub async fn build_loader(
    cwd: &str,
    agent_dir: &str,
    settings_manager: &SettingsManager,
    extension_factories: Vec<ExtensionFactory>,
) -> Arc<dyn ResourceLoader> {
    let loader: Arc<dyn ResourceLoader> =
        Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
            cwd: cwd.to_string(),
            agent_dir: agent_dir.to_string(),
            settings_manager: Some(settings_manager.clone()),
            extension_factories: extension_factories
                .into_iter()
                .map(kiana_ext_api::types::InlineExtension::Factory)
                .collect(),
            no_skills: true,
            no_prompt_templates: true,
            no_themes: true,
            no_context_files: true,
            ..Default::default()
        }));
    loader.reload(None).await.expect("resource loader reload");
    loader
}

/// 不加载任何资源的加载器。
pub async fn empty_loader(
    cwd: &str,
    agent_dir: &str,
    settings_manager: &SettingsManager,
) -> Arc<dyn ResourceLoader> {
    build_loader(cwd, agent_dir, settings_manager, Vec::new()).await
}

/// `getBlockImages` 等设置来自 settings JSON。
pub fn settings(json_value: serde_json::Value) -> Settings {
    serde_json::from_value(json_value).expect("settings")
}

/// 捕获到的请求选项摘要（`SimpleStreamOptions` 不是 `Clone`，只能挑字段）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CapturedOptions {
    pub timeout_ms: Option<u64>,
    pub websocket_connect_timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub max_retry_delay_ms: Option<u64>,
    pub identity: Option<kiana_ai::types::ClientIdentity>,
    pub headers: Option<kiana_ai::types::ProviderHeaders>,
    pub session_id: Option<String>,
    pub has_on_response: bool,
    pub has_on_provider_stream_event: bool,
}

impl CapturedOptions {
    pub fn from_options(options: &SimpleStreamOptions) -> Self {
        CapturedOptions {
            timeout_ms: options.base.timeout_ms,
            websocket_connect_timeout_ms: options.base.websocket_connect_timeout_ms,
            max_retries: options.base.max_retries,
            max_retry_delay_ms: options.base.max_retry_delay_ms,
            identity: options.base.identity.clone(),
            headers: options.base.headers.clone(),
            session_id: options.base.session_id.clone(),
            has_on_response: options.base.on_response.is_some(),
            has_on_provider_stream_event: options.base.on_provider_stream_event.is_some(),
        }
    }
}

/// 捕获 provider 请求选项的 `ProviderStreams`。
pub struct CaptureStreams {
    pub options: Mutex<Vec<CapturedOptions>>,
    pub base_urls: Mutex<Vec<String>>,
    pub providers: Mutex<Vec<String>>,
    pub response: Mutex<Option<AssistantMessage>>,
}

impl Default for CaptureStreams {
    fn default() -> Self {
        CaptureStreams {
            options: Mutex::new(Vec::new()),
            base_urls: Mutex::new(Vec::new()),
            providers: Mutex::new(Vec::new()),
            response: Mutex::new(None),
        }
    }
}

impl CaptureStreams {
    pub fn last_options(&self) -> Option<CapturedOptions> {
        lock(&self.options).last().cloned()
    }

    pub fn last_base_url(&self) -> Option<String> {
        lock(&self.base_urls).last().cloned()
    }

    pub fn set_response(&self, message: AssistantMessage) {
        *lock(&self.response) = Some(message);
    }

    pub fn call_count(&self) -> usize {
        lock(&self.options).len()
    }

    fn record(&self, model: &Model) -> AssistantMessageEventStream {
        lock(&self.base_urls).push(model.base_url.clone());
        lock(&self.providers).push(model.provider.clone());
        let message = lock(&self.response)
            .clone()
            .unwrap_or_else(|| faux_assistant_message("ok", Default::default()));
        self.finish(message)
    }

    fn finish(&self, message: AssistantMessage) -> AssistantMessageEventStream {
        let stream = create_assistant_message_event_stream();
        stream.end(message);
        stream
    }
}

impl ProviderStreams for CaptureStreams {
    fn stream(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: StreamOptions,
    ) -> AssistantMessageEventStream {
        self.record(model)
    }

    fn stream_simple(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        lock(&self.options).push(CapturedOptions::from_options(&options));
        self.record(model)
    }
}

/// 注册一个捕获型 API 实现（替代 `model.api` 的真实网络实现）。
pub fn register_capture_api(api: &str) -> Arc<CaptureStreams> {
    let capture = Arc::new(CaptureStreams::default());
    kiana_ai::compat::register_api_provider(
        kiana_ai::compat::ApiProvider::from_streams(
            api,
            Arc::clone(&capture) as Arc<dyn ProviderStreams>,
        ),
        None,
    );
    capture
}

/// 会话消息文本（用户/assistant/toolResult/system）。
pub fn message_text(message: &kiana_agent_core::types::AgentMessage) -> String {
    use kiana_agent_core::types::AgentMessage;
    match message {
        AgentMessage::Standard(Message::User(user)) => match &user.content {
            kiana_ai::types::UserContent::Text(text) => text.clone(),
            kiana_ai::types::UserContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    UserContentBlock::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
        },
        AgentMessage::Standard(Message::Assistant(assistant)) => assistant
            .content
            .iter()
            .filter_map(|block| match block {
                kiana_ai::types::AssistantContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        AgentMessage::Standard(Message::ToolResult(result)) => result
            .content
            .iter()
            .filter_map(|block| match block {
                UserContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        AgentMessage::Standard(Message::System(system)) => match &system.content {
            kiana_ai::types::SystemContent::Text(text) => text.clone(),
            kiana_ai::types::SystemContent::Blocks(blocks) => blocks
                .iter()
                .map(|block| block.text.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        },
        AgentMessage::Custom(object) => match object.get("content") {
            Some(serde_json::Value::String(text)) => text.clone(),
            Some(serde_json::Value::Array(blocks)) => blocks
                .iter()
                .filter_map(|block| {
                    if block.get("type").and_then(serde_json::Value::as_str) == Some("text") {
                        block
                            .get("text")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(""),
            _ => String::new(),
        },
    }
}

/// 订阅会话事件并返回共享事件列表。
pub fn subscribe_events(session: &AgentSession) -> Arc<Mutex<Vec<AgentSessionEvent>>> {
    let events: Arc<Mutex<Vec<AgentSessionEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let _ = session.subscribe(Arc::new(move |event| {
        lock(&sink).push(event.clone());
    }));
    events
}

/// 显式 thinking level 的便捷常量。
pub const THINKING_OFF: ModelThinkingLevel = ModelThinkingLevel::Off;

// ============================================================================
// runtime 夹具
// ============================================================================

use kiana_ext_api::events::{SessionStartEvent, SessionStartEventType, SessionStartReason};
use kiana_ext_api::types::{
    InlineExtension, ProviderChatModelConfig, ProviderConfig, ProviderModelConfig,
    ProviderRegistration,
};
use kiana_session::agent_session::ExtensionBindings;
use kiana_session::agent_session_runtime::{
    AgentSessionRuntime, AgentSessionRuntimeError, CreateAgentSessionRuntimeFactory,
    CreateAgentSessionRuntimeResult, create_agent_session_runtime,
};
use kiana_session::agent_session_services::{
    CreateAgentSessionFromServicesOptions, CreateAgentSessionServicesOptions,
    create_agent_session_from_services, create_agent_session_services,
};

/// 把 faux 模型转换成扩展 provider 的模型配置。
fn faux_provider_model_config(model: &Model) -> ProviderModelConfig {
    ProviderModelConfig::Chat(ProviderChatModelConfig {
        id: model.id.clone(),
        name: model.name.clone(),
        api: Some(model.api.clone()),
        base_url: Some(model.base_url.clone()),
        input: model.input.clone(),
        input_limits: model.input_limits.clone(),
        cost: model.cost.clone(),
        headers: None,
        model_type: None,
        reasoning: model.reasoning.unwrap_or(false),
        thinking_level_map: model.thinking_level_map.clone(),
        prompt_cache: model.prompt_cache.clone(),
        context_window: model.context_window.unwrap_or(0),
        max_tokens: model.max_tokens.unwrap_or(0),
        sampling_params: None,
        compat: None,
    })
}

/// 注册 faux provider 的扩展工厂（runtime 测试的 `kiana.registerProvider(...)`）。
pub fn faux_provider_factory(faux: &FauxCore) -> ExtensionFactory {
    let provider_id = faux.provider().to_string();
    let api = faux.api().to_string();
    let base_url = faux
        .models()
        .first()
        .map(|model| model.base_url.clone())
        .unwrap_or_default();
    let models: Vec<ProviderModelConfig> = faux
        .models()
        .iter()
        .map(faux_provider_model_config)
        .collect();
    Arc::new(move |kiana| {
        kiana.register_provider(ProviderRegistration::Config {
            name: provider_id.clone(),
            config: ProviderConfig {
                name: Some(provider_id.clone()),
                base_url: Some(base_url.clone()),
                api_key: Some("faux-key".to_string()),
                api: Some(api.clone()),
                stream_simple: None,
                images: None,
                classifiers: None,
                headers: None,
                auth_header: None,
                models: Some(models.clone()),
                refresh_models: None,
                oauth: None,
            },
        });
        Box::pin(async {})
    })
}

/// runtime 夹具选项。
pub struct RuntimeHostOptions {
    pub extension_factory: Option<ExtensionFactory>,
    pub cwd: Option<String>,
    pub session_manager: Option<SessionManager>,
    pub bootstrap_model: bool,
}

impl Default for RuntimeHostOptions {
    fn default() -> Self {
        RuntimeHostOptions {
            extension_factory: None,
            cwd: None,
            session_manager: None,
            bootstrap_model: true,
        }
    }
}

/// 通过 `createAgentSessionRuntime` 装配的测试宿主。
pub struct RuntimeHost {
    pub runtime: AgentSessionRuntime,
    pub faux: FauxCore,
    pub temp_dir: tempfile::TempDir,
    pub cwd: String,
    pub agent_dir: String,
    pub model: Model,
    pub _engine: TestEngine,
}

impl RuntimeHost {
    /// `runtime.session.bindExtensions({})`。
    pub async fn bind_extensions(&self) {
        self.runtime
            .session()
            .bind_extensions(ExtensionBindings::default())
            .await;
    }

    pub fn set_responses(&self, responses: Vec<kiana_ai::providers::faux::FauxResponseStep>) {
        self.faux.set_responses(responses);
    }

    pub fn append_responses(&self, responses: Vec<kiana_ai::providers::faux::FauxResponseStep>) {
        self.faux.append_responses(responses);
    }

    pub async fn dispose(&self) {
        self.runtime.dispose().await;
    }
}

/// 创建 runtime 宿主（对应 `test/suite/agent-session-runtime.test.ts#createRuntimeForTest`）。
pub async fn runtime_host(options: RuntimeHostOptions) -> RuntimeHost {
    let engine = install_engine();
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let cwd = match &options.cwd {
        Some(cwd) => cwd.clone(),
        None => temp_dir.path().to_string_lossy().into_owned(),
    };
    std::fs::create_dir_all(&cwd).expect("cwd");
    let agent_dir = temp_dir.path().join("agent").to_string_lossy().into_owned();
    std::fs::create_dir_all(&agent_dir).expect("agent dir");

    let faux = create_faux_core(RegisterFauxProviderOptions {
        models: Some(vec![
            FauxModelDefinition {
                id: "faux-1".to_string(),
                reasoning: Some(true),
                ..Default::default()
            },
            FauxModelDefinition {
                id: "faux-2".to_string(),
                reasoning: Some(false),
                ..Default::default()
            },
        ]),
        ..Default::default()
    });
    let api = faux.api().to_string();
    kiana_ai::compat::register_api_provider(
        kiana_ai::compat::ApiProvider::from_streams(&api, Arc::new(faux.clone())),
        None,
    );
    let model = faux.get_model(None).cloned().expect("faux model");
    let bootstrap_model = options.bootstrap_model.then(|| model.clone());

    let provider_factory = faux_provider_factory(&faux);
    let user_factory = options.extension_factory.clone();
    let factory_agent_dir = agent_dir.clone();
    let factory: CreateAgentSessionRuntimeFactory = Arc::new(move |input| {
        let provider_factory = Arc::clone(&provider_factory);
        let user_factory = user_factory.clone();
        let agent_dir = factory_agent_dir.clone();
        let bootstrap_model = bootstrap_model.clone();
        Box::pin(async move {
            let mut factories: Vec<InlineExtension> =
                vec![InlineExtension::Factory(provider_factory)];
            if let Some(user_factory) = user_factory {
                factories.push(InlineExtension::Factory(user_factory));
            }
            let services = create_agent_session_services(CreateAgentSessionServicesOptions {
                cwd: input.cwd.clone(),
                agent_dir: Some(agent_dir),
                resource_loader_options: Some(DefaultResourceLoaderOptions {
                    extension_factories: factories,
                    no_skills: true,
                    no_prompt_templates: true,
                    no_themes: true,
                    no_context_files: true,
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .map_err(|error| AgentSessionRuntimeError::Other(error.message))?;
            let created =
                create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
                    services: services.clone(),
                    session_manager: input.session_manager,
                    session_start_event: input.session_start_event,
                    model: bootstrap_model.clone(),
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

    let session_manager = options.session_manager.unwrap_or_else(|| {
        // 显式使用临时目录下的会话目录：绝不落到真实的 `~/.kiana/agent/sessions`。
        let session_dir = temp_dir.path().join("sessions");
        SessionManager::create(&cwd, Some(session_dir.to_string_lossy().as_ref()), None)
            .expect("session manager")
    });
    let runtime = create_agent_session_runtime(
        factory,
        kiana_session::agent_session_runtime::CreateAgentSessionRuntimeOptions {
            cwd: cwd.clone(),
            agent_dir: agent_dir.clone(),
            session_manager,
            session_start_event: None::<SessionStartEvent>,
        },
    )
    .await
    .expect("runtime");
    RuntimeHost {
        runtime,
        faux,
        temp_dir,
        cwd,
        agent_dir,
        model,
        _engine: engine,
    }
}

/// 手动构造的 runtime（回归 #8724 用）。
pub fn runtime_from_session(
    session: AgentSession,
    services: kiana_session::agent_session_services::AgentSessionServices,
    create_runtime: CreateAgentSessionRuntimeFactory,
) -> AgentSessionRuntime {
    AgentSessionRuntime::new(session, services, create_runtime, Vec::new(), None)
}

// 避免未使用导入（faux 夹具中的辅助类型）。
#[allow(dead_code)]
fn _unused_session_start(_: SessionStartEventType, _: SessionStartReason) {}

/// 最小工具执行器：返回文本 `ok`（扩展注册工具用）。
pub struct OkToolExecutor;

impl kiana_ext_api::tool_types::ToolDefinitionExecute for OkToolExecutor {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: serde_json::Value,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
        _ctx: std::sync::Arc<dyn kiana_ext_api::types::ExtensionToolContextApi>,
    ) -> kiana_agent_core::types::BoxFuture<
        'a,
        Result<
            kiana_agent_core::types::AgentToolResult<serde_json::Value>,
            kiana_agent_core::types::AgentToolError,
        >,
    > {
        Box::pin(async move {
            Ok(kiana_agent_core::types::AgentToolResult {
                content: vec![UserContentBlock::Text(kiana_ai::types::TextContent {
                    kind_text: kiana_ai::types::TextKind::Text,
                    text: "ok".to_string(),
                    text_signature: None,
                })],
                details: serde_json::Value::Null,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

/// [`OkToolExecutor`] 的便捷构造。
pub fn ok_tool_executor() -> OkToolExecutor {
    OkToolExecutor
}
