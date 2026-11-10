//! `test/sdk-stream-options.test.ts`：createAgentSession 的请求选项与 cache warming。
#![allow(clippy::arc_with_non_send_sync)]

mod session_support;

use std::sync::{Arc, Mutex};

use kiana_ai::event_stream::create_assistant_message_event_stream;
use kiana_ai::providers::faux::faux_assistant_message;
use kiana_ai::types::{
    AssistantMessage, Model, ModelCost, ModelInputModality, PromptCacheTier, StopReason,
};
use kiana_config::auth::storage::{AuthStorage, auth_data_from_value};
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use kiana_config::settings::SettingsManager;
use kiana_ext_api::events::ExtensionEvent;
use kiana_ext_api::types::ExtensionFactory;
use kiana_resources::loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader,
};
use kiana_session::agent_session::PromptOptions;
use kiana_session::sdk::{CreateAgentSessionOptions, create_agent_session};
use kiana_session_core::session_manager::SessionManager;
use serde_json::json;

use session_support::{lock, run, settings};

fn capture_model(api: &str) -> Model {
    let mut model = Model {
        id: "capture-model".to_string(),
        name: "Capture Model".to_string(),
        api: api.to_string(),
        provider: "capture-provider".to_string(),
        base_url: "https://capture.invalid/v1".to_string(),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        ..Default::default()
    };
    model.headers = Some(
        [("x-model".to_string(), "model".to_string())]
            .into_iter()
            .collect(),
    );
    model.reasoning = Some(false);
    model.context_window = Some(128000);
    model.max_tokens = Some(4096);
    model
}

fn done_message(api: &str, prompt_tokens: u64) -> AssistantMessage {
    let mut message = faux_assistant_message("ok", Default::default());
    message.api = api.to_string();
    message.provider = "capture-provider".to_string();
    message.model = "capture-model".to_string();
    message.usage.cache_read = prompt_tokens;
    message.usage.total_tokens = prompt_tokens;
    message.stop_reason = StopReason::Stop;
    message
}

struct CaptureState {
    options: Mutex<Vec<session_support::CapturedOptions>>,
    headers: Mutex<Vec<Option<kiana_ai::types::ProviderHeaders>>>,
}

impl CaptureState {
    fn last(&self) -> session_support::CapturedOptions {
        lock(&self.options)
            .last()
            .cloned()
            .expect("captured options")
    }
}

async fn capture_stream_options(
    api: &str,
    settings_json: serde_json::Value,
    extension_factory: Option<ExtensionFactory>,
    provider_event: Option<serde_json::Value>,
    provider_response: Option<kiana_ai::types::ProviderResponse>,
) -> (
    session_support::CapturedOptions,
    Option<kiana_ai::types::ProviderHeaders>,
) {
    run(async move {
        let _engine = session_support::install_engine();
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let cwd = temp_dir.path().join("project");
        let agent_dir = temp_dir.path().join("agent");
        std::fs::create_dir_all(&cwd).expect("cwd");
        std::fs::create_dir_all(&agent_dir).expect("agent dir");
        let cwd = cwd.to_string_lossy().into_owned();
        let agent_dir = agent_dir.to_string_lossy().into_owned();
        let model = capture_model(api);
        let settings_manager = SettingsManager::in_memory(settings(settings_json));
        let loader: Arc<dyn ResourceLoader> =
            Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
                cwd: cwd.clone(),
                agent_dir: agent_dir.clone(),
                settings_manager: Some(settings_manager.clone()),
                extension_factories: extension_factory
                    .into_iter()
                    .map(kiana_ext_api::types::InlineExtension::Factory)
                    .collect(),
                no_skills: true,
                no_prompt_templates: true,
                no_themes: true,
                no_context_files: true,
                ..Default::default()
            }));
        loader.reload(None).await.expect("reload");

        let auth_storage = AuthStorage::in_memory(auth_data_from_value(json!({
            "capture-provider": { "type": "api_key", "key": "test-api-key" }
        })));
        let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(Arc::new(auth_storage)),
            models_path: ModelsPathOption::Disabled,
            allow_model_network: false,
            refresh_on_create: Some(false),
            ..Default::default()
        })
        .await
        .expect("runtime");

        let state = Arc::new(CaptureState {
            options: Mutex::new(Vec::new()),
            headers: Mutex::new(Vec::new()),
        });
        let provider_state = Arc::clone(&state);
        let provider_model = model.clone();
        let provider_model_for_stream = provider_model.clone();
        let provider_event_for_stream = provider_event.clone();
        let provider_response_for_stream = provider_response.clone();
        model_runtime
            .register_provider(
                "capture-provider",
                kiana_config::models::composer::ProviderConfigInput {
                    base_url: Some(model.base_url.clone()),
                    api: Some(api.to_string()),
                    headers: Some(
                        [("x-provider".to_string(), "provider".to_string())]
                            .into_iter()
                            .collect(),
                    ),
                    stream_simple: Some(Arc::new(move |_model, _context, options| {
                        lock(&provider_state.options)
                            .push(session_support::CapturedOptions::from_options(&options));
                        lock(&provider_state.headers).push(options.base.headers.clone());
                        if let Some(response) = &provider_response_for_stream
                            && let Some(callback) = options.base.on_response.clone()
                        {
                            let response = response.clone();
                            callback(response, &provider_model_for_stream);
                        }
                        if let Some(event) = &provider_event_for_stream
                            && let Some(callback) = options.base.on_provider_stream_event.clone()
                        {
                            let event = event.clone();
                            let model = provider_model_for_stream.clone();
                            callback(event, &model);
                        }
                        let stream = create_assistant_message_event_stream();
                        stream.end(done_message("openai-completions", 0));
                        stream
                    })),
                    ..Default::default()
                },
            )
            .expect("register provider");

        let session_manager = SessionManager::in_memory(&cwd, None, None).expect("session");
        let result = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(cwd.clone()),
            agent_dir: Some(agent_dir.clone()),
            model: Some(model.clone()),
            model_runtime: Some(model_runtime),
            settings_manager: Some(settings_manager),
            session_manager: Some(session_manager),
            resource_loader: Some(loader),
            ..Default::default()
        })
        .await
        .expect("create session");
        let session = result.session;

        // TS 在 provider 内部 `await` onResponse/onProviderStreamEvent；派发必须已经完成，
        // 因此这里不再需要让出 LocalSet。
        session
            .prompt("test".to_string(), PromptOptions::default())
            .await
            .expect("prompt");
        let captured = state.last();
        let headers = lock(&state.headers).last().cloned().unwrap_or(None);
        session.dispose();
        (captured, headers)
    })
    .await
}

#[tokio::test]
async fn defaults_timeout_ms_from_http_idle_timeout_for_all_providers() {
    let (options, _) = capture_stream_options(
        "openai-completions",
        json!({ "httpIdleTimeoutMs": 1234 }),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(options.timeout_ms, Some(1234));
}

#[tokio::test]
async fn forwards_websocket_connect_timeout_from_settings() {
    let (options, _) = capture_stream_options(
        "openai-codex-responses",
        json!({ "websocketConnectTimeoutMs": 1234 }),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(options.websocket_connect_timeout_ms, Some(1234));
}

#[tokio::test]
async fn forwards_provider_retry_settings() {
    let (options, _) = capture_stream_options(
        "openai-completions",
        json!({ "retry": { "provider": { "maxRetries": 2, "maxRetryDelayMs": 3000 } } }),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(options.max_retries, Some(2));
    assert_eq!(options.max_retry_delay_ms, Some(3000));
}

#[tokio::test]
async fn forwards_provider_stream_events_to_extensions() {
    use kiana_ext_api::events::ExtensionEvent;
    let events: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let factory: ExtensionFactory = Arc::new(move |kiana| {
        let sink = Arc::clone(&sink);
        let _ = kiana.on(
            kiana_ext_api::events::ExtensionEventKind::ProviderStreamEvent,
            kiana_ext_api::events::ExtensionHandler::Standard(Arc::new(move |event, _ctx| {
                let sink = Arc::clone(&sink);
                Box::pin(async move {
                    if let ExtensionEvent::ProviderStreamEvent(event) = event {
                        lock(&sink).push(json!({
                            "data": event.data,
                            "type": "provider_stream_event",
                            "provider": event.provider,
                            "api": event.api,
                            "model": event.model,
                        }));
                    }
                    Ok::<_, Box<dyn std::error::Error + Send + Sync>>(None)
                })
            })),
        );
        Box::pin(async {})
    });
    let (options, _) = capture_stream_options(
        "openai-completions",
        json!({}),
        Some(factory),
        Some(json!({ "openrouter_metadata": { "strategy": "direct" } })),
        None,
    )
    .await;
    assert!(options.has_on_provider_stream_event);
    assert_eq!(
        *lock(&events),
        vec![json!({
            "data": { "openrouter_metadata": { "strategy": "direct" } },
            "type": "provider_stream_event",
            "provider": "capture-provider",
            "api": "openai-completions",
            "model": "capture-model",
        })]
    );
}

#[tokio::test]
async fn forwards_provider_responses_to_extensions_before_the_response_completes() {
    // TS sdk.ts 的 handleProviderResponse 是 async 且被 provider await：handler 必须在
    // 流结束（prompt 返回）之前完成。这里故意不再让出 LocalSet，直接断言已观察到事件。
    use kiana_ext_api::events::ExtensionEvent;
    let events: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let factory: ExtensionFactory = Arc::new(move |kiana| {
        let sink = Arc::clone(&sink);
        let _ = kiana.on(
            kiana_ext_api::events::ExtensionEventKind::AfterProviderResponse,
            kiana_ext_api::events::ExtensionHandler::Standard(Arc::new(move |event, _ctx| {
                let sink = Arc::clone(&sink);
                Box::pin(async move {
                    if let ExtensionEvent::AfterProviderResponse(event) = event {
                        lock(&sink).push(json!({
                            "type": "after_provider_response",
                            "status": event.status,
                            "headers": event.headers,
                        }));
                    }
                    Ok::<_, Box<dyn std::error::Error + Send + Sync>>(None)
                })
            })),
        );
        Box::pin(async {})
    });
    let headers = [("x-upstream".to_string(), "yes".to_string())]
        .into_iter()
        .collect();
    let (options, _) = capture_stream_options(
        "openai-completions",
        json!({}),
        Some(factory),
        None,
        Some(kiana_ai::types::ProviderResponse {
            status: 201,
            headers,
        }),
    )
    .await;
    assert!(options.has_on_response);
    assert_eq!(
        *lock(&events),
        vec![json!({
            "type": "after_provider_response",
            "status": 201,
            "headers": { "x-upstream": "yes" },
        })]
    );
}

#[tokio::test]
async fn runs_before_provider_headers_on_assembled_headers() {
    let called: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
    let sink = Arc::clone(&called);
    let factory: ExtensionFactory = Arc::new(move |kiana| {
        let sink = Arc::clone(&sink);
        let _ = kiana.on(
            kiana_ext_api::events::ExtensionEventKind::BeforeProviderHeaders,
            kiana_ext_api::events::ExtensionHandler::Standard(Arc::new(move |event, _ctx| {
                let sink = Arc::clone(&sink);
                Box::pin(async move {
                    if let ExtensionEvent::BeforeProviderHeaders(event) = event {
                        assert_eq!(
                            event
                                .headers
                                .get("x-provider")
                                .and_then(Clone::clone)
                                .as_deref(),
                            Some("provider")
                        );
                        assert_eq!(
                            event
                                .headers
                                .get("x-model")
                                .and_then(Clone::clone)
                                .as_deref(),
                            Some("model")
                        );
                        *lock(&sink) += 1;
                    }
                    Ok::<_, Box<dyn std::error::Error + Send + Sync>>(None)
                })
            })),
        );
        Box::pin(async {})
    });
    let (_, headers) =
        capture_stream_options("openai-completions", json!({}), Some(factory), None, None).await;
    // Rust 原生 handler 接收的是 owned event（不像 JS 那样共享可变对象），所以这里断言
    // handler 以组装后的 provider 头被调用；in-place 的 `x-hook` 写回只有 JS 扩展能观察。
    assert_eq!(*lock(&called), 1);
    let headers = headers.expect("headers");
    assert_eq!(
        headers.get("x-provider").and_then(Clone::clone).as_deref(),
        Some("provider")
    );
    assert_eq!(
        headers.get("x-model").and_then(Clone::clone).as_deref(),
        Some("model")
    );
    assert!(!headers.contains_key("x-hook"));
}

// ============================================================================
// cache warming
// ============================================================================

#[tokio::test]
async fn waits_for_the_next_request_instead_of_restoring_cache_warming() {
    run(async {
        let _engine = session_support::install_engine();
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let cwd = temp_dir.path().to_string_lossy().into_owned();
        let agent_dir = temp_dir.path().join("agent");
        std::fs::create_dir_all(&agent_dir).expect("agent dir");
        let agent_dir = agent_dir.to_string_lossy().into_owned();
        let mut model = capture_model("anthropic-messages");
        model.cost = ModelCost {
            input: 10.0,
            output: 50.0,
            cache_read: 0.25,
            cache_write: 12.5,
            tiers: None,
        };
        model.prompt_cache = Some([(PromptCacheTier::Short, 300.0)].into_iter().collect());
        let settings_manager =
            SettingsManager::in_memory(settings(json!({ "cacheWarming": "idle" })));

        let calls = Arc::new(Mutex::new(0_u32));
        let calls_for_stream = Arc::clone(&calls);
        let auth_storage = AuthStorage::in_memory(auth_data_from_value(json!({
            "capture-provider": { "type": "api_key", "key": "test-api-key" }
        })));
        let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(Arc::new(auth_storage)),
            models_path: ModelsPathOption::Disabled,
            allow_model_network: false,
            refresh_on_create: Some(false),
            ..Default::default()
        })
        .await
        .expect("runtime");
        model_runtime
            .register_provider(
                "capture-provider",
                kiana_config::models::composer::ProviderConfigInput {
                    base_url: Some(model.base_url.clone()),
                    api: Some(model.api.clone()),
                    stream_simple: Some(Arc::new(move |_model, _context, _options| {
                        *lock(&calls_for_stream) += 1;
                        let stream = create_assistant_message_event_stream();
                        stream.end(done_message("anthropic-messages", 100_000));
                        stream
                    })),
                    ..Default::default()
                },
            )
            .expect("register provider");

        let mut session_manager = SessionManager::in_memory(&cwd, None, None).expect("session");
        session_manager
            .append_model_change("capture-provider", "capture-model")
            .expect("model change");
        session_manager
            .append_thinking_level_change("off")
            .expect("thinking");
        session_manager
            .append_message(kiana_agent_core::types::AgentMessage::Standard(
                kiana_ai::types::Message::User(kiana_ai::types::UserMessage {
                    role: kiana_ai::types::UserRole::User,
                    content: kiana_ai::types::UserContent::Text("test".to_string()),
                    timestamp: 0,
                }),
            ))
            .expect("user");
        let assistant = done_message("anthropic-messages", 100_000);
        session_manager
            .append_message(kiana_agent_core::types::AgentMessage::Standard(
                kiana_ai::types::Message::Assistant(assistant.clone()),
            ))
            .expect("assistant");
        session_manager
            .append_usage(
                "cache_warm",
                "capture-provider",
                "capture-model",
                assistant.usage.clone(),
                None,
            )
            .expect("usage");

        let loader: Arc<dyn ResourceLoader> =
            Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
                cwd: cwd.clone(),
                agent_dir: agent_dir.clone(),
                settings_manager: Some(settings_manager.clone()),
                no_skills: true,
                no_prompt_templates: true,
                no_themes: true,
                no_context_files: true,
                ..Default::default()
            }));
        loader.reload(None).await.expect("reload");
        let result = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(cwd.clone()),
            agent_dir: Some(agent_dir.clone()),
            model: Some(model.clone()),
            model_runtime: Some(model_runtime),
            settings_manager: Some(settings_manager),
            session_manager: Some(session_manager),
            resource_loader: Some(loader),
            ..Default::default()
        })
        .await
        .expect("create session");
        let session = result.session;
        assert_eq!(*lock(&calls), 0);
        let status = session
            .cache_warming_status()
            .expect("cache warming status");
        assert_eq!(
            status.state,
            kiana_config::cache::warmer::CacheWarmingState::Inactive
        );
        assert_eq!(status.reason.as_deref(), Some("waiting for first request"));
        session.dispose();
    })
    .await;
}

#[tokio::test]
async fn schedules_cache_warming_after_a_completed_session_request() {
    run(async {
        let _engine = session_support::install_engine();
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let cwd = temp_dir.path().to_string_lossy().into_owned();
        let agent_dir = temp_dir.path().join("agent");
        std::fs::create_dir_all(&agent_dir).expect("agent dir");
        let agent_dir = agent_dir.to_string_lossy().into_owned();
        let mut model = capture_model("anthropic-messages");
        model.cost = ModelCost {
            input: 10.0,
            output: 50.0,
            cache_read: 0.25,
            cache_write: 12.5,
            tiers: None,
        };
        model.prompt_cache = Some([(PromptCacheTier::Short, 300.0)].into_iter().collect());
        let settings_manager =
            SettingsManager::in_memory(settings(json!({ "cacheWarming": "idle" })));
        let loader: Arc<dyn ResourceLoader> =
            Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
                cwd: cwd.clone(),
                agent_dir: agent_dir.clone(),
                settings_manager: Some(settings_manager.clone()),
                no_skills: true,
                no_prompt_templates: true,
                no_themes: true,
                no_context_files: true,
                ..Default::default()
            }));
        loader.reload(None).await.expect("reload");
        let auth_storage = AuthStorage::in_memory(auth_data_from_value(json!({
            "capture-provider": { "type": "api_key", "key": "test-api-key" }
        })));
        let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(Arc::new(auth_storage)),
            models_path: ModelsPathOption::Disabled,
            allow_model_network: false,
            refresh_on_create: Some(false),
            ..Default::default()
        })
        .await
        .expect("runtime");
        let calls = Arc::new(Mutex::new(0_u32));
        let calls_for_stream = Arc::clone(&calls);
        model_runtime
            .register_provider(
                "capture-provider",
                kiana_config::models::composer::ProviderConfigInput {
                    base_url: Some(model.base_url.clone()),
                    api: Some(model.api.clone()),
                    stream_simple: Some(Arc::new(move |_model, _context, _options| {
                        *lock(&calls_for_stream) += 1;
                        let stream = create_assistant_message_event_stream();
                        stream.end(done_message("anthropic-messages", 100_000));
                        stream
                    })),
                    ..Default::default()
                },
            )
            .expect("register provider");
        let session_manager = SessionManager::in_memory(&cwd, None, None).expect("session");
        let result = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(cwd.clone()),
            agent_dir: Some(agent_dir.clone()),
            model: Some(model.clone()),
            model_runtime: Some(model_runtime),
            settings_manager: Some(settings_manager),
            session_manager: Some(session_manager),
            resource_loader: Some(loader),
            ..Default::default()
        })
        .await
        .expect("create session");
        let session = result.session;
        session
            .prompt("test".to_string(), PromptOptions::default())
            .await
            .expect("prompt");
        let status = session
            .cache_warming_status()
            .expect("cache warming status");
        // TS 断言 `nextWarmAt > Date.now()`。
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis() as i64;
        assert!(status.next_warm_at.expect("next warm at") > now_ms);
        session.dispose();
    })
    .await;
}
