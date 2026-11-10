//! `test/sdk-client-identity.test.ts`：createAgentSession 的请求身份。
//!
//! 两个 OpenCode session header 用例已由 `kiana-config/tests/model_runtime_auth_options.rs` 覆盖，
//! 这里只移植通过真实会话的 User-Agent / attribution 三例。
#![allow(clippy::arc_with_non_send_sync)]

mod session_support;

use std::sync::{Arc, Mutex, OnceLock};

use kiana_ai::event_stream::create_assistant_message_event_stream;
use kiana_ai::providers::faux::faux_assistant_message;
use kiana_ai::types::{Model, ModelCost, ModelInputModality, StopReason};
use kiana_config::auth::storage::{AuthStorage, auth_data_from_value};
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use kiana_config::settings::SettingsManager;
use kiana_session::agent_session::PromptOptions;
use kiana_session::sdk::{CreateAgentSessionOptions, create_agent_session};
use kiana_session_core::session_manager::SessionManager;
use serde_json::json;

use session_support::{CapturedOptions, empty_loader, run};

fn env_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn create_model(provider: &str, base_url: &str) -> Model {
    Model {
        id: format!("{provider}-test-model"),
        name: format!("{provider} Test Model"),
        api: "openai-completions".to_string(),
        provider: provider.to_string(),
        base_url: base_url.to_string(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(128000),
        max_tokens: Some(4096),
        ..Default::default()
    }
}

async fn capture_options(
    model: Model,
    telemetry_enabled: Option<bool>,
    session_id: Option<String>,
) -> CapturedOptions {
    run(async move {
        let _engine = session_support::install_engine();
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let cwd = temp_dir.path().join("project");
        let agent_dir = temp_dir.path().join("agent");
        std::fs::create_dir_all(&cwd).expect("cwd");
        std::fs::create_dir_all(&agent_dir).expect("agent dir");
        let cwd = cwd.to_string_lossy().into_owned();
        let agent_dir = agent_dir.to_string_lossy().into_owned();

        let settings_manager = SettingsManager::in_memory(Default::default());
        if telemetry_enabled == Some(false) {
            settings_manager.set_enable_install_telemetry(false);
        }
        let loader = empty_loader(&cwd, &agent_dir, &settings_manager).await;
        let auth_storage = AuthStorage::in_memory(auth_data_from_value(json!({
            model.provider.clone(): { "type": "api_key", "key": "test-api-key" }
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
        let captured: Arc<Mutex<Vec<CapturedOptions>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        model_runtime
            .register_provider(
                &model.provider,
                kiana_config::models::composer::ProviderConfigInput {
                    base_url: Some(model.base_url.clone()),
                    api: Some(model.api.clone()),
                    stream_simple: Some(Arc::new(move |_model, _context, options| {
                        sink.lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .push(CapturedOptions::from_options(&options));
                        let stream = create_assistant_message_event_stream();
                        let mut message = faux_assistant_message("ok", Default::default());
                        message.api = "openai-completions".to_string();
                        message.provider = "capture-provider".to_string();
                        message.model = "capture-model".to_string();
                        message.stop_reason = StopReason::Stop;
                        stream.end(message);
                        stream
                    })),
                    ..Default::default()
                },
            )
            .expect("register provider");

        let mut session_manager = SessionManager::in_memory(&cwd, None, None).expect("session");
        if let Some(session_id) = &session_id {
            session_manager
                .new_session(Some(
                    kiana_session_core::session_manager::NewSessionOptions {
                        id: Some(session_id.clone()),
                        parent_session: None,
                    },
                ))
                .expect("new session");
        }
        let result = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(cwd.clone()),
            agent_dir: Some(agent_dir.clone()),
            model: Some(model),
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
        let options = captured
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .last()
            .cloned()
            .expect("captured options");
        session.dispose();
        options
    })
    .await
}

#[tokio::test]
async fn identifies_kiana_with_a_detailed_user_agent_and_attribution_as_a_cli_agent() {
    let _guard = env_lock().lock().await;
    unsafe {
        std::env::remove_var("KIANA_TELEMETRY");
    }
    let options = capture_options(
        create_model("openrouter", "https://openrouter.ai/api/v1"),
        None,
        None,
    )
    .await;
    let identity = options.identity.expect("identity");
    assert_eq!(
        identity.user_agent,
        Some(kiana_ai::types::UserAgentMode::Detailed(
            kiana_ai::types::UserAgentModeDetailed::Detailed
        ))
    );
    assert_eq!(
        identity.version.as_deref(),
        Some(kiana_core::config::VERSION)
    );
    assert_eq!(
        identity
            .attribution
            .as_ref()
            .and_then(|attribution| attribution.as_ref()),
        Some(&*kiana_ai::utils::identity::KIANA_ATTRIBUTION)
    );
    let target = kiana_ai::utils::identity::ClientIdentityTarget::new(
        "openrouter",
        "https://openrouter.ai/api/v1",
    );
    let headers = kiana_ai::utils::identity::get_attribution_headers(&target, Some(&identity));
    assert_eq!(
        headers
            .get("HTTP-Referer")
            .and_then(Clone::clone)
            .as_deref(),
        Some("https://kiana.dev")
    );
    assert_eq!(
        headers
            .get("X-OpenRouter-Title")
            .and_then(Clone::clone)
            .as_deref(),
        Some("kiana")
    );
    assert_eq!(
        headers
            .get("X-OpenRouter-Categories")
            .and_then(Clone::clone)
            .as_deref(),
        Some("cli-agent")
    );
}

#[tokio::test]
async fn turns_attribution_off_when_telemetry_is_disabled() {
    let _guard = env_lock().lock().await;
    unsafe {
        std::env::remove_var("KIANA_TELEMETRY");
    }
    let options = capture_options(
        create_model("nvidia", "https://integrate.api.nvidia.com/v1"),
        Some(false),
        None,
    )
    .await;
    let identity = options.identity.expect("identity");
    assert_eq!(
        identity.user_agent,
        Some(kiana_ai::types::UserAgentMode::Detailed(
            kiana_ai::types::UserAgentModeDetailed::Detailed
        ))
    );
    assert_eq!(
        identity.version.as_deref(),
        Some(kiana_core::config::VERSION)
    );
    assert_eq!(identity.attribution, Some(None));
    let target = kiana_ai::utils::identity::ClientIdentityTarget::new(
        "nvidia",
        "https://integrate.api.nvidia.com/v1",
    );
    assert!(
        kiana_ai::utils::identity::get_attribution_headers(&target, Some(&identity)).is_empty()
    );
}

#[tokio::test]
async fn turns_attribution_off_with_kiana_telemetry_zero() {
    let _guard = env_lock().lock().await;
    unsafe {
        std::env::set_var("KIANA_TELEMETRY", "0");
    }
    let options = capture_options(
        create_model("openrouter", "https://openrouter.ai/api/v1"),
        None,
        None,
    )
    .await;
    unsafe {
        std::env::remove_var("KIANA_TELEMETRY");
    }
    let identity = options.identity.expect("identity");
    assert_eq!(identity.attribution, Some(None));
}
