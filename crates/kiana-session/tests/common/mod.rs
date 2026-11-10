//! 共享测试夹具（对应 `packages/coding-agent/test/suite/harness.ts` 与 `test/utilities.ts`
//! 的被使用部分）。
//!
//! `#![allow(dead_code)]` 的原因：每个集成测试编译同一个 `common` 模块，但只用到其中一部分
//! 辅助函数（与 `kiana-session-core/tests/common/mod.rs` 的先例一致）。
#![allow(dead_code)]

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use kiana_agent_core::agent::{Agent, AgentInitialState, AgentOptions};
use kiana_agent_core::types::{AgentMessage, AgentTool, StreamFn};
use kiana_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, AuthError, AuthResult, ModelAuth, ProviderAuth,
};
use kiana_ai::compat::{ApiProvider, register_api_provider};
use kiana_ai::providers::faux::{
    FauxCore, FauxModelDefinition, FauxResponseStep, RegisterFauxProviderOptions, create_faux_core,
};
use kiana_ai::types::{AssistantContentBlock, Message, Model, ToolResultMessage, UserContentBlock};
use kiana_config::auth::storage::{AuthStorage, auth_data_from_value};
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use kiana_config::settings::SettingsManager;
use kiana_config::settings::schema::Settings;
use kiana_ext_api::types::ExtensionFactory;
use kiana_extensions::LoadExtensionsResult;
use kiana_extensions::engine::{self, JsEngine};
use kiana_extensions::loader::{create_extension_runtime, load_extension_from_factory};
use kiana_extensions::runner::ExtensionRunner;
use kiana_extensions::runner::{context, dispatch};
use kiana_resources::loader::{
    AgentsFilesResult, PromptsResult, ResourceExtensionPaths, ResourceLoader, ResourceLoaderError,
    ResourceLoaderReloadOptions, SkillsResult, ThemesResult,
};
use kiana_session::agent_session::{
    AgentSession, AgentSessionConfig, AgentSessionEvent, AgentSessionEventListener,
};
use kiana_session_core::messages::convert_to_llm;
use kiana_session_core::session_manager::SessionManager;
use serde_json::json;

/// 在 current_thread runtime + `LocalSet` 上运行测试体（扩展派发要求同线程）。
pub async fn run<F: std::future::Future>(future: F) -> F::Output {
    let local = tokio::task::LocalSet::new();
    local.run_until(future).await
}

/// `getMessageText`。
pub fn get_message_text(message: &AgentMessage) -> String {
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
                .join("\n"),
        },
        AgentMessage::Standard(Message::Assistant(assistant)) => assistant
            .content
            .iter()
            .filter_map(|block| match block {
                AssistantContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        AgentMessage::Standard(Message::ToolResult(result)) => result
            .content
            .iter()
            .filter_map(|block| match block {
                UserContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
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
                        Some(
                            block
                                .get("text")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        )
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        },
    }
}

pub fn get_user_texts(harness: &Harness) -> Vec<String> {
    harness
        .session
        .messages()
        .iter()
        .filter(|message| message.role() == "user")
        .map(get_message_text)
        .collect()
}

pub fn get_assistant_texts(harness: &Harness) -> Vec<String> {
    harness
        .session
        .messages()
        .iter()
        .filter(|message| message.role() == "assistant")
        .map(get_message_text)
        .collect()
}

/// 会话 transcript 里最近一次 `toolName` 的结果。
pub fn get_tool_result(harness: &Harness, tool_name: &str) -> ToolResultMessage {
    harness
        .session
        .messages()
        .into_iter()
        .rev()
        .find_map(|message| match message {
            AgentMessage::Standard(Message::ToolResult(result))
                if result.tool_name == tool_name =>
            {
                Some(result)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("No {tool_name} tool result"))
}

/// 最小资源加载器（`createTestResourceLoader`）。
pub struct TestResourceLoader {
    extensions: Mutex<LoadExtensionsResult>,
    skills: Mutex<SkillsResult>,
    prompts: Mutex<PromptsResult>,
    agents_files: Mutex<AgentsFilesResult>,
    system_prompt: Mutex<Option<String>>,
    append_system_prompt: Mutex<Vec<String>>,
}

impl TestResourceLoader {
    pub fn new(extensions: LoadExtensionsResult) -> Self {
        TestResourceLoader {
            extensions: Mutex::new(extensions),
            skills: Mutex::new(SkillsResult {
                skills: Vec::new(),
                diagnostics: Vec::new(),
            }),
            prompts: Mutex::new(PromptsResult::default()),
            agents_files: Mutex::new(AgentsFilesResult::default()),
            system_prompt: Mutex::new(None),
            append_system_prompt: Mutex::new(Vec::new()),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl ResourceLoader for TestResourceLoader {
    fn get_extensions(&self) -> LoadExtensionsResult {
        let guard = lock(&self.extensions);
        LoadExtensionsResult {
            extensions: guard.extensions.clone(),
            errors: guard.errors.clone(),
            warnings: guard.warnings.clone(),
            runtime: Arc::clone(&guard.runtime),
        }
    }
    fn get_skills(&self) -> SkillsResult {
        lock(&self.skills).clone()
    }
    fn get_prompts(&self) -> PromptsResult {
        lock(&self.prompts).clone()
    }
    fn get_themes(&self) -> ThemesResult {
        ThemesResult::default()
    }
    fn get_agents_files(&self) -> AgentsFilesResult {
        lock(&self.agents_files).clone()
    }
    fn get_system_prompt(&self) -> Option<String> {
        lock(&self.system_prompt).clone()
    }
    fn get_system_prompt_source(&self) -> Option<kiana_resources::loader::SystemPromptSource> {
        None
    }
    fn get_append_system_prompt(&self) -> Vec<String> {
        lock(&self.append_system_prompt).clone()
    }
    fn get_append_system_prompt_sources(&self) -> Vec<kiana_resources::loader::SystemPromptSource> {
        Vec::new()
    }
    fn extend_resources(&self, _paths: ResourceExtensionPaths) -> Result<(), ResourceLoaderError> {
        Ok(())
    }
    fn reload<'a>(
        &'a self,
        _options: Option<ResourceLoaderReloadOptions>,
    ) -> kiana_ext_api::LocalBoxFuture<'a, Result<(), ResourceLoaderError>> {
        Box::pin(async { Ok(()) })
    }
}

/// `createTestExtensionsResult`。
pub async fn create_test_extensions_result(
    factories: Vec<ExtensionFactory>,
    cwd: &str,
) -> LoadExtensionsResult {
    let runtime = create_extension_runtime();
    let event_bus = kiana_core::event_bus::create_event_bus();
    let mut extensions = Vec::new();
    for (index, factory) in factories.into_iter().enumerate() {
        let path = format!("<inline:{}>", index + 1);
        if let Ok(extension) = load_extension_from_factory(
            factory,
            cwd,
            event_bus.clone(),
            Arc::clone(&runtime),
            Some(&path),
        )
        .await
        {
            extensions.push(extension);
        }
    }
    LoadExtensionsResult {
        extensions,
        errors: Vec::new(),
        warnings: None,
        runtime,
    }
}

/// Harness 选项（`HarnessOptions` 的 Rust 形态）。
#[derive(Default)]
pub struct HarnessOptions {
    pub provider: Option<String>,
    pub models: Option<Vec<FauxModelDefinition>>,
    pub settings: Option<Settings>,
    /// 直接注入 `SettingsManager`（例如文件后端 + 损坏的项目设置）。优先于 `settings`。
    pub settings_manager: Option<SettingsManager>,
    pub tools: Option<Vec<AgentTool>>,
    pub initial_active_tool_names: Option<Vec<String>>,
    pub allowed_tool_names: Option<Vec<String>>,
    pub excluded_tool_names: Option<Vec<String>>,
    pub extension_factories: Vec<ExtensionFactory>,
    pub with_configured_auth: Option<bool>,
    /// 让原生 provider 的 API-key auth 检查失败（`checkAuth` rejection 回归用）：
    /// 进程内没有存储凭证，provider 的 `ApiKeyAuth::resolve` 返回该错误。
    pub check_auth_error: Option<String>,
    pub session_manager: Option<SessionManager>,
    pub agents_files: Option<Vec<kiana_ext_api::system_prompt::ContextFile>>,
    /// 覆盖 ResourceLoader 的技能列表（TS harness 的 `resourceLoader.getSkills`）。
    pub skills: Option<kiana_resources::loader::SkillsResult>,
    /// 覆盖 ResourceLoader 的 prompt 模板列表（TS harness 的 `resourceLoader.getPrompts`）。
    pub prompts: Option<kiana_resources::loader::PromptsResult>,
    /// 覆盖 `Agent` 的流函数（默认 kiana-ai `streamSimple` 的规范实例）。
    pub stream_fn: Option<StreamFn>,
    /// 第 10 周持久记忆接线（默认关闭）。
    pub memory: Option<kiana_session::memory_wiring::MemoryWiringOptions>,
    /// 模型回答语言（system prompt `output_language` section）。
    pub output_language: Option<String>,
    /// 第 11 周后半连续性接线（默认关闭）。
    pub continuity: Option<kiana_session::continuity::ContinuityOptions>,
    /// settings 显式关闭 continuity（压过 `KIANA_CONTINUITY` 环境变量）。
    pub continuity_disabled: bool,
    /// 第 9 周遥测注入（默认由 `AgentSession` 自行构造）。
    pub telemetry: Option<Arc<kiana_agent_core::SessionTelemetry>>,
    /// SDK 自定义工具定义（可带 MCP annotations）。
    pub custom_tools: Vec<kiana_extensions::ToolDefinition>,
}

/// 测试夹具（`Harness`）。
pub struct Harness {
    pub session: AgentSession,
    /// `AgentSession` 内部 `Agent` 的克隆（`Agent::clone` 共享状态，对应 TS `harness.session.agent`）。
    pub agent: Agent,
    pub settings_manager: SettingsManager,
    pub auth_storage: AuthStorage,
    pub faux: FauxCore,
    pub models: Vec<Model>,
    pub events: Arc<Mutex<Vec<AgentSessionEvent>>>,
    pub temp_dir: tempfile::TempDir,
    extension_runner_ref: Arc<Mutex<Option<Arc<ExtensionRunner>>>>,
    _resource_loader: Arc<dyn ResourceLoader>,
    /// 扩展 JS 引擎（派发需要；保持到 harness 结束且在创建线程上 Drop）。
    _engine: Rc<JsEngine>,
}

impl Harness {
    pub fn get_model(&self, model_id: Option<&str>) -> Option<Model> {
        self.faux.get_model(model_id).cloned()
    }

    pub fn set_responses(&self, responses: Vec<FauxResponseStep>) {
        self.faux.set_responses(responses);
    }

    pub fn append_responses(&self, responses: Vec<FauxResponseStep>) {
        self.faux.append_responses(responses);
    }

    pub fn get_pending_response_count(&self) -> usize {
        self.faux.get_pending_response_count()
    }

    pub fn events_of_type(&self, event_type: &str) -> Vec<AgentSessionEvent> {
        lock(&self.events)
            .iter()
            .filter(|event| event_type_name(event) == event_type)
            .cloned()
            .collect()
    }

    pub fn cleanup(&self) {
        self.session.dispose();
    }

    /// TS 里 `harness.sessionManager.appendMessage(...)` 的等价物（并刷新会话上下文）。
    pub fn append_session_message(&self, message: AgentMessage) -> String {
        let id = self
            .session
            .with_session_manager_mut(|manager| manager.append_message(message))
            .expect("append message");
        self.session.refresh_context();
        id
    }

    /// TS 里 `harness.sessionManager.appendCustomEntry(...)`。
    pub fn append_custom_entry(
        &self,
        custom_type: &str,
        data: Option<serde_json::Value>,
    ) -> String {
        let id = self
            .session
            .with_session_manager_mut(|manager| manager.append_custom_entry(custom_type, data))
            .expect("append custom entry");
        self.session.refresh_context();
        id
    }

    /// 会话视图（`harness.sessionManager`）。
    pub fn session_manager(
        &self,
    ) -> Arc<dyn kiana_session_core::session_manager::ReadonlySessionManager> {
        self.session.session_manager()
    }

    /// 会话条目（`harness.sessionManager.getEntries()`）。
    pub fn session_entries(&self) -> Vec<kiana_session_core::session_manager::SessionEntry> {
        self.session.session_manager().get_entries()
    }
}

/// `AgentSessionEvent` 的 `type` 字面量（对齐 serde 的 tag 值）。
pub fn event_type_name(event: &AgentSessionEvent) -> &'static str {
    match event {
        AgentSessionEvent::AgentStart => "agent_start",
        AgentSessionEvent::AgentEnd { .. } => "agent_end",
        AgentSessionEvent::AgentSettled { .. } => "agent_settled",
        AgentSessionEvent::QueueUpdate { .. } => "queue_update",
        AgentSessionEvent::CompactionStart { .. } => "compaction_start",
        AgentSessionEvent::CompactionEnd { .. } => "compaction_end",
        AgentSessionEvent::EntryAppended { .. } => "entry_appended",
        AgentSessionEvent::SessionInfoChanged { .. } => "session_info_changed",
        AgentSessionEvent::ThinkingLevelChanged { .. } => "thinking_level_changed",
        AgentSessionEvent::AutoRetryStart { .. } => "auto_retry_start",
        AgentSessionEvent::AutoRetryEnd { .. } => "auto_retry_end",
        AgentSessionEvent::SummarizationRetryScheduled { .. } => "summarization_retry_scheduled",
        AgentSessionEvent::SummarizationRetryAttemptStart { .. } => {
            "summarization_retry_attempt_start"
        }
        AgentSessionEvent::SummarizationRetryFinished => "summarization_retry_finished",
        AgentSessionEvent::Condensation { .. } => "condensation",
        AgentSessionEvent::HookNotification { .. } => "hook_notification",
        AgentSessionEvent::BashExecutionUpdate { .. } => "bash_execution_update",
        AgentSessionEvent::PermissionRequested { .. } => "permission_requested",
        AgentSessionEvent::PermissionDecided { .. } => "permission_decided",
        AgentSessionEvent::PermissionDenied { .. } => "permission_denied",
        AgentSessionEvent::TurnStart => "turn_start",
        AgentSessionEvent::TurnEnd { .. } => "turn_end",
        AgentSessionEvent::MessageStart { .. } => "message_start",
        AgentSessionEvent::MessageUpdate { .. } => "message_update",
        AgentSessionEvent::MessageEnd { .. } => "message_end",
        AgentSessionEvent::ToolExecutionStart { .. } => "tool_execution_start",
        AgentSessionEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
        AgentSessionEvent::ToolExecutionEnd { .. } => "tool_execution_end",
    }
}

/// `createHarness`。
pub async fn create_harness(options: HarnessOptions) -> Harness {
    let engine = Rc::new(JsEngine::new().expect("js engine"));
    engine::install(&engine).expect("engine prelude");
    context::install(&engine).expect("context.js");
    dispatch::install(Rc::clone(&engine)).expect("dispatch.js");
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let cwd = temp_dir.path().to_string_lossy().into_owned();
    let faux = create_faux_core(RegisterFauxProviderOptions {
        provider: options.provider.clone(),
        models: options.models.clone(),
        ..Default::default()
    });
    let api = faux.api().to_string();
    let provider_id = faux.provider().to_string();
    let models: Vec<Model> = faux.models().to_vec();
    let model = models.first().cloned().expect("faux provider model");

    // API 实现注册（`stream_simple` 按 `model.api` 分派）。
    register_api_provider(
        ApiProvider::from_streams(&api, Arc::new(faux.clone())),
        None,
    );

    let check_auth_error = options.check_auth_error.clone();
    let with_configured_auth =
        options.with_configured_auth.unwrap_or(true) && check_auth_error.is_none();
    let auth_storage = AuthStorage::in_memory(auth_data_from_value(if with_configured_auth {
        json!({ provider_id.clone(): { "type": "api_key", "key": "faux-key" } })
    } else {
        json!({})
    }));
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(auth_storage.clone())),
        models_path: ModelsPathOption::Disabled,
        allow_model_network: false,
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
    .expect("model runtime");

    // 原生 provider（与 TS harness 的 `modelRegistry.registerProvider` 对应）。
    let provider = kiana_ai::models::provider::create_provider(
        kiana_ai::models::provider::CreateProviderOptions {
            id: provider_id.clone(),
            name: Some(provider_id.clone()),
            base_url: Some(model.base_url.clone()),
            headers: None,
            auth: ProviderAuth {
                api_key: if let Some(message) = &check_auth_error {
                    Some(Arc::new(FailingApiKeyAuth {
                        message: message.clone(),
                    }))
                } else if with_configured_auth {
                    Some(Arc::new(FauxApiKeyAuth))
                } else {
                    None
                },
                oauth: None,
            },
            models: models
                .iter()
                .cloned()
                .map(kiana_ai::types::AnyModel::Chat)
                .collect(),
            fetch_models: None,
            filter_models: None,
            filter_all_models: None,
            api: Some(kiana_ai::models::provider::ProviderApiImpl::Single(
                Arc::new(faux.clone()),
            )),
            images: None,
            classifiers: None,
        },
    )
    .expect("faux provider");
    let _ = runtime.register_native_provider(provider);
    if with_configured_auth {
        for _ in 0..200 {
            let _ = runtime
                .refresh(kiana_ai::models::ModelsRefreshOptions {
                    allow_network: Some(false),
                    ..Default::default()
                })
                .await;
            if runtime.has_configured_auth(&provider_id) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    }

    let session_manager = options
        .session_manager
        .unwrap_or_else(|| SessionManager::in_memory(&cwd, None, None).expect("session"));
    let settings_manager = match options.settings_manager {
        Some(manager) => manager,
        None => SettingsManager::in_memory(options.settings.unwrap_or_default()),
    };
    let tool_map: Option<indexmap::IndexMap<String, AgentTool>> = options.tools.map(|tools| {
        tools
            .into_iter()
            .map(|tool| (tool.name.clone(), tool))
            .collect()
    });
    let extension_runner_ref: Arc<Mutex<Option<Arc<ExtensionRunner>>>> = Arc::new(Mutex::new(None));

    // TS harness 把 `onPayload`/`onResponse`/`transformContext` 转发给扩展（见
    // `test/suite/harness.ts`）。扩展派发是 `!Send`，因此经 `kiana-session` 的会话线程桥
    //（`run_on_session_thread*`）执行。
    let payload_runner = Arc::clone(&extension_runner_ref);
    let response_runner = Arc::clone(&extension_runner_ref);
    let transform_runner = Arc::clone(&extension_runner_ref);
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some(String::new()),
            model: Some(model.clone()),
            thinking_level: None,
            tools: Some(Vec::new()),
            messages: None,
        }),
        stream_fn: Some(options.stream_fn.clone().unwrap_or_else(stream_fn)),
        convert_to_llm: Some(Arc::new(|messages: Vec<AgentMessage>| {
            convert_to_llm(&messages)
        })),
        get_api_key: Some(Arc::new(move |_provider: String| {
            let configured = with_configured_auth;
            Box::pin(async move { configured.then(|| "faux-key".to_string()) })
        })),
        on_payload: Some(Arc::new(move |payload, _model| {
            let runner = lock(&payload_runner).clone();
            let Some(runner) = runner else {
                return Some(payload);
            };
            if !runner.has_handlers("before_provider_request") {
                return Some(payload);
            }
            kiana_session::agent_session::run_on_session_thread_blocking(async move {
                runner.emit_before_provider_request(payload).await
            })
            .and_then(Result::ok)
        })),
        on_response: Some(Arc::new(move |response, _model| {
            let runner = lock(&response_runner).clone();
            let Some(runner) = runner else {
                return;
            };
            if !runner.has_handlers("after_provider_response") {
                return;
            }
            let event = kiana_ext_api::events::ExtensionEvent::AfterProviderResponse(
                kiana_ext_api::events::AfterProviderResponseEvent {
                    event_type:
                        kiana_ext_api::events::AfterProviderResponseEventType::AfterProviderResponse,
                    status: response.status,
                    headers: response.headers,
                },
            );
            kiana_session::agent_session::spawn_on_session_thread(async move {
                let _ = runner.emit(event).await;
            });
        })),
        transform_context: Some(Arc::new(move |messages, _signal| {
            let runner = lock(&transform_runner).clone();
            Box::pin(async move {
                let Some(runner) = runner else {
                    return messages;
                };
                kiana_session::agent_session::run_on_session_thread(async move {
                    runner.emit_context(messages).await
                })
                .await
                .and_then(Result::ok)
                .unwrap_or_default()
            })
        })),
        ..Default::default()
    })
    .expect("agent");

    let extensions_result = if options.extension_factories.is_empty() {
        LoadExtensionsResult {
            extensions: Vec::new(),
            errors: Vec::new(),
            warnings: None,
            runtime: create_extension_runtime(),
        }
    } else {
        create_test_extensions_result(options.extension_factories, &cwd).await
    };
    #[allow(clippy::arc_with_non_send_sync)] // ResourceLoader 是单线程 trait
    let resource_loader: Arc<dyn ResourceLoader> = {
        let loader = TestResourceLoader::new(extensions_result);
        if let Some(agents_files) = options.agents_files.clone() {
            *lock(&loader.agents_files) = AgentsFilesResult { agents_files };
        }
        if let Some(skills) = options.skills.clone() {
            *lock(&loader.skills) = skills;
        }
        if let Some(prompts) = options.prompts.clone() {
            *lock(&loader.prompts) = prompts;
        }
        Arc::new(loader)
    };

    let agent_handle = agent.clone();
    let session = AgentSession::new(AgentSessionConfig {
        agent,
        session_manager,
        settings_manager: settings_manager.clone(),
        cwd: cwd.clone(),
        scoped_models: Vec::new(),
        resource_loader: resource_loader.clone(),
        custom_tools: options.custom_tools.clone(),
        model_runtime: runtime,
        cache_warmer: None,
        initial_active_tool_names: options.initial_active_tool_names.clone(),
        uses_default_tools: Some(options.initial_active_tool_names.is_none()),
        default_tool_modifiers: Vec::new(),
        allowed_tool_names: options.allowed_tool_names.clone(),
        excluded_tool_names: options.excluded_tool_names.clone(),
        base_tools_override: tool_map,
        extension_runner_ref: Some(Arc::clone(&extension_runner_ref)),
        session_start_event: None,
        memory: options.memory.clone(),
        output_language: options.output_language.clone(),
        continuity: options.continuity.clone(),
        continuity_disabled: options.continuity_disabled,
        telemetry: options.telemetry.clone(),
    });

    let events: Arc<Mutex<Vec<AgentSessionEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let listener: AgentSessionEventListener = Arc::new(move |event| {
        lock(&sink).push(event.clone());
    });
    let _ = session.subscribe(listener);

    Harness {
        session,
        agent: agent_handle,
        settings_manager,
        auth_storage,
        faux: faux.clone(),
        models,
        events,
        temp_dir,
        extension_runner_ref,
        _resource_loader: resource_loader,
        _engine: engine,
    }
}

/// `checkAuth` 会 rejection 的 api-key auth（评审 finding #5 的回归夹具）。
struct FailingApiKeyAuth {
    message: String,
}

#[async_trait::async_trait]
impl ApiKeyAuth for FailingApiKeyAuth {
    fn name(&self) -> &str {
        "Failing"
    }

    async fn resolve(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        Err(AuthError::new(self.message.clone()))
    }
}

/// 测试 provider 的 api-key auth（TS harness 的 `apiKey: "faux-key"`）。
struct FauxApiKeyAuth;

#[async_trait::async_trait]
impl ApiKeyAuth for FauxApiKeyAuth {
    fn name(&self) -> &str {
        "Faux"
    }

    async fn resolve(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        Ok(Some(AuthResult::new(ModelAuth {
            api_key: Some("faux-key".to_string()),
            headers: None,
            base_url: None,
        })))
    }
}

fn stream_fn() -> StreamFn {
    // TS harness 直接传 `streamSimple`；用 kiana-session 的规范实例保持
    // `_getSummarizationRequestAuth` 的函数身份判断。
    kiana_session::agent_session::stream_simple_stream_fn()
}

/// 便捷：faux 文本响应。
pub fn faux_text_response(text: impl Into<String>) -> FauxResponseStep {
    FauxResponseStep::Message(kiana_ai::providers::faux::faux_assistant_message(
        text.into(),
        Default::default(),
    ))
}

/// 便捷：faux 工具调用响应。
pub fn faux_tool_response(
    tool_call: kiana_ai::types::ToolCall,
    stop_reason: kiana_ai::types::StopReason,
) -> FauxResponseStep {
    FauxResponseStep::Message(kiana_ai::providers::faux::faux_assistant_message(
        AssistantContentBlock::ToolCall(tool_call),
        kiana_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(stop_reason),
            ..Default::default()
        },
    ))
}

// ============================================================================
// 扩展与工具辅助（`ExtensionAPI` / `AgentTool` 的测试封装）
// ============================================================================

use kiana_ext_api::events::{
    ExtensionEvent, ExtensionEventResult, ExtensionHandler, ExtensionHandlerFn,
};
use kiana_ext_api::types::{ExtensionApi, ExtensionContextApi};
use serde_json::Value as JsonValue;

pub type HandlerResult =
    Result<Option<ExtensionEventResult>, Box<dyn std::error::Error + Send + Sync>>;

/// 标准事件 handler。
pub fn standard<F>(handler: F) -> ExtensionHandler
where
    F: Fn(
            ExtensionEvent,
            Arc<dyn ExtensionContextApi>,
        ) -> kiana_ext_api::LocalBoxFuture<'static, HandlerResult>
        + 'static,
{
    ExtensionHandler::Standard(Arc::new(handler) as ExtensionHandlerFn)
}

/// 单次调用的扩展工厂（TS 的 `(kiana) => { ... }`）。
pub fn factory<F>(f: F) -> ExtensionFactory
where
    F: FnOnce(Arc<dyn ExtensionApi>) + 'static,
{
    let f = Arc::new(Mutex::new(Some(f)));
    Arc::new(move |kiana| {
        if let Some(f) = lock(&f).take() {
            f(kiana);
        }
        Box::pin(async {})
    })
}

/// 响应式等待一个事件（轮询；测试内部使用）。
pub async fn wait_for_event(
    events: &Arc<Mutex<Vec<AgentSessionEvent>>>,
    event_type: &str,
) -> AgentSessionEvent {
    for _ in 0..2000 {
        if let Some(event) = lock(events)
            .iter()
            .find(|event| event_type_name(event) == event_type)
            .cloned()
        {
            return event;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    panic!("timed out waiting for {event_type}");
}

/// 测试工具：等待 `release.notify_one()` 后返回文本。
pub struct WaitTool {
    pub notify: Arc<tokio::sync::Notify>,
    pub text: String,
}

impl WaitTool {
    pub fn make(text: impl Into<String>) -> (AgentTool, Arc<tokio::sync::Notify>) {
        let notify = Arc::new(tokio::sync::Notify::new());
        let tool = AgentTool {
            name: "wait".to_string(),
            label: "Wait".to_string(),
            description: "Wait for release".to_string(),
            parameters: json!({"type": "object", "properties": {}}),
            constrained_sampling: None,
            output_schema: None,
            prepare_arguments: None,
            execution_mode: None,
            replay: None,
            execute: Arc::new(WaitTool {
                notify: Arc::clone(&notify),
                text: text.into(),
            }),
        };
        (tool, notify)
    }
}

impl kiana_agent_core::types::AgentToolExecute for WaitTool {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: JsonValue,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
    ) -> kiana_agent_core::types::BoxFuture<
        'a,
        Result<
            kiana_agent_core::types::AgentToolResult<JsonValue>,
            kiana_agent_core::types::AgentToolError,
        >,
    > {
        let notify = Arc::clone(&self.notify);
        let text = self.text.clone();
        Box::pin(async move {
            notify.notified().await;
            Ok(kiana_agent_core::types::AgentToolResult {
                content: vec![UserContentBlock::Text(kiana_ai::types::TextContent {
                    kind_text: kiana_ai::types::TextKind::Text,
                    text,
                    text_signature: None,
                })],
                details: JsonValue::Null,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

/// 把 text 响应工厂的上下文用户文本读出来。
pub fn user_texts_in_context(context: &kiana_ai::types::Context) -> Vec<String> {
    context
        .messages
        .iter()
        .filter(|message| message.role() == "user")
        .map(|message| match message {
            Message::User(user) => match &user.content {
                kiana_ai::types::UserContent::Text(text) => text.clone(),
                kiana_ai::types::UserContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|block| match block {
                        UserContentBlock::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            },
            _ => String::new(),
        })
        .collect()
}

/// 从 JSON 构造 `Settings`（测试里只写与用例相关的字段）。
pub fn settings_from_json(value: serde_json::Value) -> Settings {
    serde_json::from_value(value).expect("settings")
}

/// 最小 `ToolDefinition`（扩展在 `session_start` 里注册用）。
pub fn simple_tool_definition(
    name: &str,
    description: &str,
    prompt_snippet: Option<&str>,
) -> kiana_extensions::ToolDefinition {
    struct OkExecute;

    impl kiana_ext_api::tool_types::ToolDefinitionExecute for OkExecute {
        fn execute<'a>(
            &'a self,
            _tool_call_id: &'a str,
            _params: JsonValue,
            _signal: Option<&'a tokio_util::sync::CancellationToken>,
            _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
            _ctx: Arc<dyn kiana_ext_api::types::ExtensionToolContextApi>,
        ) -> kiana_agent_core::types::BoxFuture<
            'a,
            Result<
                kiana_agent_core::types::AgentToolResult<JsonValue>,
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
                    details: JsonValue::Null,
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }
    }

    kiana_extensions::ToolDefinition {
        name: name.to_string(),
        label: name.to_string(),
        description: description.to_string(),
        prompt_snippet: prompt_snippet.map(str::to_string),
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
        execute: Arc::new(OkExecute),
        render_call: None,
        render_result: None,
    }
}

/// 最小用户消息（utilities.ts#userMsg）。
pub fn user_msg(text: &str) -> AgentMessage {
    AgentMessage::Standard(Message::User(kiana_ai::types::UserMessage {
        role: kiana_ai::types::UserRole::User,
        content: kiana_ai::types::UserContent::Text(text.to_string()),
        timestamp: kiana_ai::model_catalog::now_epoch_millis() as i64,
    }))
}

/// 最小 assistant 消息（utilities.ts#assistantMsg）。
pub fn assistant_msg(text: &str) -> AgentMessage {
    let mut message = kiana_ai::providers::faux::faux_assistant_message(text, Default::default());
    message.provider = "anthropic".to_string();
    message.model = "test".to_string();
    AgentMessage::Standard(Message::Assistant(message))
}

/// 在当前线程同步轮询一个非 `Send` future（测试里调用 `navigateTree` 等）。
pub fn poll_block<F: std::future::Future>(future: F) -> F::Output {
    use std::task::{Context, Poll};

    let waker = std::task::Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

// ============================================================================
// 审批 UI（结构化权限测试用）
// ============================================================================

use std::collections::VecDeque;

use kiana_core::event_bus::Unsubscribe;
use kiana_ext_api::LocalBoxFuture;
use kiana_ext_api::ui::{
    AutocompleteProviderFactory, CustomFactory, CustomOptions, EditorFactory,
    ExtensionUIDialogOptions, ExtensionUiContext, ExtensionWidgetOptions, FooterFactory,
    HeaderFactory, NotifyKind, TerminalInputHandler, ThemeInfo, ThemeOrName, ThemeResult,
    WidgetContent, WorkingIndicatorOptions,
};
use kiana_theme::theme::Theme;

/// 可编排的 UI 上下文：`select`/`input` 按队列返回，其余方法空操作。
///
/// 用队列而不是单一返回值，是为了覆盖「Allow once 后再问一次」「Always allow 后不再问」
/// 这类多轮审批。
pub struct ScriptedUi {
    selections: Mutex<VecDeque<Option<String>>>,
    inputs: Mutex<VecDeque<Option<String>>>,
    pub select_titles: Mutex<Vec<String>>,
    /// 每次 `select` 收到的选项（与 `select_titles` 一一对应）。
    pub select_options: Mutex<Vec<Vec<String>>>,
    theme: Arc<Theme>,
}

impl ScriptedUi {
    pub fn new(selections: Vec<Option<String>>, inputs: Vec<Option<String>>) -> Arc<Self> {
        Arc::new(Self {
            selections: Mutex::new(selections.into()),
            inputs: Mutex::new(inputs.into()),
            select_titles: Mutex::new(Vec::new()),
            select_options: Mutex::new(Vec::new()),
            theme: kiana_theme::theme::get_theme_by_name("system")
                .unwrap_or_else(|| kiana_theme::theme::get_theme_by_name("dark").expect("theme")),
        })
    }

    pub fn select_count(&self) -> usize {
        lock(&self.select_titles).len()
    }
}

impl ExtensionUiContext for ScriptedUi {
    fn select(
        &self,
        title: &str,
        options: &[String],
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, Option<String>> {
        lock(&self.select_titles).push(title.to_string());
        lock(&self.select_options).push(options.to_vec());
        let answer = lock(&self.selections).pop_front().unwrap_or(None);
        Box::pin(async move { answer })
    }

    fn confirm(
        &self,
        _title: &str,
        _message: &str,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, bool> {
        Box::pin(async { false })
    }

    fn input(
        &self,
        _title: &str,
        _placeholder: Option<&str>,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, Option<String>> {
        let answer = lock(&self.inputs).pop_front().unwrap_or(None);
        Box::pin(async move { answer })
    }

    fn notify(&self, _message: &str, _kind: Option<NotifyKind>) {}

    fn on_terminal_input(&self, _handler: TerminalInputHandler) -> Unsubscribe {
        Box::new(|| {})
    }

    fn set_status(&self, _key: &str, _text: Option<&str>) {}

    fn set_working_message(&self, _message: Option<&str>) {}

    fn set_working_visible(&self, _visible: bool) {}

    fn set_working_indicator(&self, _options: Option<WorkingIndicatorOptions>) {}

    fn set_hidden_thinking_label(&self, _label: Option<&str>) {}

    fn set_widget(
        &self,
        _key: &str,
        _content: Option<WidgetContent>,
        _options: Option<ExtensionWidgetOptions>,
    ) {
    }

    fn set_footer(&self, _factory: Option<FooterFactory>) {}

    fn set_header(&self, _factory: Option<HeaderFactory>) {}

    fn set_title(&self, _title: &str) {}

    fn custom(
        &self,
        _factory: CustomFactory,
        _options: Option<CustomOptions>,
    ) -> LocalBoxFuture<'_, Box<dyn std::any::Any + Send>> {
        Box::pin(async { Box::new(()) as Box<dyn std::any::Any + Send> })
    }

    fn paste_to_editor(&self, _text: &str) {}

    fn set_editor_text(&self, _text: &str) {}

    fn get_editor_text(&self) -> String {
        String::new()
    }

    fn editor(&self, _title: &str, _prefill: Option<&str>) -> LocalBoxFuture<'_, Option<String>> {
        Box::pin(async { None })
    }

    fn add_autocomplete_provider(&self, _factory: AutocompleteProviderFactory) {}

    fn set_editor_component(&self, _factory: Option<EditorFactory>) {}

    fn get_editor_component(&self) -> Option<EditorFactory> {
        None
    }

    fn theme(&self) -> &Theme {
        self.theme.as_ref()
    }

    fn get_all_themes(&self) -> Vec<ThemeInfo> {
        Vec::new()
    }

    fn get_theme(&self, _name: &str) -> Option<Theme> {
        None
    }

    fn set_theme(&self, _theme: ThemeOrName) -> ThemeResult {
        ThemeResult {
            success: false,
            error: Some("UI not available".to_string()),
        }
    }

    fn get_tools_expanded(&self) -> bool {
        false
    }

    fn set_tools_expanded(&self, _expanded: bool) {}
}
