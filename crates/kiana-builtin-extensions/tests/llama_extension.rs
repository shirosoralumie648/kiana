//! `packages/coding-agent/test/llama-extension.test.ts` 的 Rust 移植（15 个 `it`）。
//!
//! 第一个测试用宿主提供的 fake [`ExtensionApi`]（模式同 `crates/kiana-ext-api/tests/public_surface.rs`
//! 的 `MockExtensionApi`）直接调用工厂；其余直接驱动 `create_llama_provider` / `LlamaClient` /
//! `HuggingFaceClient`，HTTP 全部走 [`llama_support`] 的真实回环服务器（无 mock fetch）。

mod llama_support;

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use kiana_ai::auth::types::{ApiKeyAuthInput, ApiKeyCredential, AuthResult, Credential, ModelAuth};
use kiana_ai::models::{ModelsStoreEntry, RefreshModelsContext};
use kiana_ai::types::{AnyModel, ClassifierContext, ClassifierQuestion, Model, ModelInputModality};
use kiana_ai::utils::abort::AbortScope;
use kiana_builtin_extensions::llama::client::{
    LlamaClient, LlamaModelArchitecture, LlamaModelInfo, LlamaModelMeta, LlamaModelStatusInfo,
    llama_inference_url, normalize_llama_server_url,
};
use kiana_builtin_extensions::llama::huggingface::{
    HuggingFaceClient, HuggingFaceGated, HuggingFaceModel, HuggingFaceQuantization,
    find_huggingface_token,
};
use kiana_builtin_extensions::llama::index::llama_extension;
use kiana_builtin_extensions::llama::provider::{
    LLAMA_PROVIDER_ID, LlamaProviderController, create_llama_provider,
};
use kiana_config::settings::Settings;
use kiana_core::event_bus::{EventBus, Unsubscribe};
use kiana_core::mcp_servers::RegisteredMcpServer;
use kiana_core::slash_commands::SlashCommandInfo;
use kiana_ext_api::events::{ExtensionEventKind, ExtensionHandler};
use kiana_ext_api::types::{
    ExtensionVirtualModel, FlagValue, ProviderRegistration, RegisterCommandOptions,
    RegisterFlagOptions, RegisterShortcutOptions, SendMessageOptions, SendUserMessageOptions,
};
use kiana_ext_api::{
    EntryRenderer, ExecOptions, ExecResult, ExtensionApi, MarkdownTransformer, MessageRenderer,
    ToolDefinition, ToolInfo, ToolRendererResolver,
};
use llama_support::{
    EmptyAuthContext, FakeInteraction, SseStreams, TestRequest, listen, publish_into, publish_ok,
    wait_ms,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// fake ExtensionApi（只记录 provider/command 注册）
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FakeApi {
    provider_ids: Mutex<Vec<String>>,
    commands: Mutex<IndexMap<String, RegisterCommandOptions>>,
}

impl ExtensionApi for FakeApi {
    fn on(&self, _kind: ExtensionEventKind, _handler: ExtensionHandler) -> Unsubscribe {
        Box::new(|| {})
    }
    fn register_tool(&self, _tool: ToolDefinition) {}
    fn register_command(&self, name: &str, options: RegisterCommandOptions) {
        self.commands
            .lock()
            .unwrap()
            .insert(name.to_string(), options);
    }
    fn register_shortcut(&self, _shortcut: &str, _options: RegisterShortcutOptions) {}
    fn register_flag(&self, _name: &str, _options: RegisterFlagOptions) {}
    fn get_flag(&self, _name: &str) -> Option<FlagValue> {
        None
    }
    fn register_message_renderer(&self, _custom_type: &str, _renderer: MessageRenderer) {}
    fn register_markdown_transformer(&self, _transformer: MarkdownTransformer) {}
    fn register_entry_renderer(&self, _custom_type: &str, _renderer: EntryRenderer) {}
    fn register_tool_renderer(&self, _resolver: ToolRendererResolver) {}
    fn send_message(&self, _options: SendMessageOptions) {}
    fn send_user_message(
        &self,
        _content: kiana_ai::types::UserContent,
        _options: Option<SendUserMessageOptions>,
    ) {
    }
    fn append_entry(&self, _custom_type: &str, _data: Option<kiana_ai::types::JsonValue>) {}
    fn set_session_name(&self, _name: &str) {}
    fn get_session_name(&self) -> Option<String> {
        None
    }
    fn set_label(&self, _entry_id: &str, _label: Option<&str>) {}
    fn exec<'a>(
        &'a self,
        _command: &'a str,
        _args: &'a [String],
        _options: Option<ExecOptions>,
    ) -> kiana_ext_api::LocalBoxFuture<'a, ExecResult> {
        Box::pin(async {
            ExecResult {
                stdout: String::new(),
                stderr: String::new(),
                code: 0,
                killed: false,
            }
        })
    }
    fn get_active_tools(&self) -> Vec<String> {
        Vec::new()
    }
    fn get_all_tools(&self) -> Vec<ToolInfo> {
        Vec::new()
    }
    fn get_settings(&self) -> Settings {
        Settings::default()
    }
    fn set_active_tools(&self, _tool_names: Vec<String>) {}
    fn get_commands(&self) -> Vec<SlashCommandInfo> {
        Vec::new()
    }
    fn set_model(&self, _model: Model) -> kiana_ext_api::LocalBoxFuture<'static, bool> {
        Box::pin(async { false })
    }
    fn get_thinking_level(&self) -> kiana_agent_core::ThinkingLevel {
        kiana_agent_core::ThinkingLevel::default()
    }
    fn set_thinking_level(&self, _level: kiana_agent_core::ThinkingLevel) {}
    fn register_provider(&self, registration: ProviderRegistration) {
        if let ProviderRegistration::Native(provider) = registration {
            self.provider_ids
                .lock()
                .unwrap()
                .push(provider.id().to_string());
        }
    }
    fn unregister_provider(&self, _name: &str) {}
    fn register_mcp_server(&self, _name: &str, _config: kiana_core::mcp_servers::McpServerConfig) {}
    fn unregister_mcp_server(&self, _name: &str) {}
    fn get_mcp_servers(&self) -> Vec<RegisteredMcpServer> {
        Vec::new()
    }
    fn register_virtual_model(&self, _model: ExtensionVirtualModel) {}
    fn unregister_virtual_model(&self, _provider: &str, _id: &str) {}
    fn events(&self) -> Arc<dyn EventBus> {
        kiana_core::event_bus::create_event_bus()
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn model_info(id: &str, status: &str) -> LlamaModelInfo {
    LlamaModelInfo {
        id: id.to_string(),
        status: LlamaModelStatusInfo {
            value: status.to_string(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn api_key_credential(server_url: &str) -> Credential {
    let mut env = IndexMap::new();
    env.insert("LLAMA_BASE_URL".to_string(), server_url.to_string());
    Credential::ApiKey(ApiKeyCredential {
        key: Some("local".to_string()),
        env: Some(env),
        extra: IndexMap::new(),
    })
}

fn refresh_context(
    credential: Option<Credential>,
    stored: Option<ModelsStoreEntry>,
    publish: kiana_ai::models::PublishFn,
    allow_network: bool,
) -> RefreshModelsContext {
    RefreshModelsContext {
        credential,
        stored,
        publish,
        allow_network,
        force: None,
        signal: AbortScope::new(),
        base_url: None,
    }
}

fn classifier_model(controller: &LlamaProviderController) -> Model {
    controller
        .provider
        .get_all_models()
        .into_iter()
        .find_map(|model| match model {
            AnyModel::Classifier(model) => Some(model),
            _ => None,
        })
        .expect("classifier model")
}

fn json_object(value: Value) -> IndexMap<String, Value> {
    value
        .as_object()
        .expect("object")
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[test]
fn registers_a_native_provider_and_llama_command() {
    let api = Arc::new(FakeApi::default());
    llama_extension(api.clone());
    let commands = api.commands.lock().unwrap();
    assert_eq!(
        commands
            .get("llama")
            .and_then(|command| command.description.as_deref()),
        Some("Manage llama.cpp router models")
    );
    drop(commands);
    assert_eq!(
        api.provider_ids.lock().unwrap().as_slice(),
        &[LLAMA_PROVIDER_ID.to_string()]
    );
}

#[test]
fn normalizes_management_and_inference_urls() {
    assert_eq!(
        normalize_llama_server_url("http://127.0.0.1:8080/v1/").unwrap(),
        "http://127.0.0.1:8080"
    );
    assert_eq!(
        normalize_llama_server_url("https://example.com/prefix/v1").unwrap(),
        "https://example.com/prefix"
    );
    let error = normalize_llama_server_url("file:///tmp/llama").unwrap_err();
    assert!(error.message.contains("http or https"));
    assert!(
        llama_inference_url("http://h:8080")
            .unwrap()
            .ends_with("/v1")
    );
}

#[test]
fn exposes_loaded_and_sleeping_models_with_router_metadata() {
    let controller = create_llama_provider();
    let mut loaded = model_info("loaded", "loaded");
    loaded.status.args = json!(["llama-server", "--n-gpu-layers", "999"]);
    loaded.architecture = Some(LlamaModelArchitecture {
        input_modalities: vec!["text".to_string(), "image".to_string()],
        output_modalities: Vec::new(),
    });
    loaded.meta = Some(LlamaModelMeta {
        n_ctx: Some(65536.0),
        n_ctx_train: Some(131072.0),
        ..Default::default()
    });
    controller.set_catalog(
        &[
            loaded,
            model_info("sleeping", "sleeping"),
            model_info("unloaded", "unloaded"),
            model_info("loading", "loading"),
        ],
        "http://localhost:8080",
        false,
    );
    let models = controller.provider.get_models();
    assert_eq!(
        models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        vec!["loaded", "sleeping"]
    );
    assert_eq!(models[0].base_url, "http://localhost:8080/v1");
    assert_eq!(models[0].context_window, Some(65536));
    assert_eq!(models[0].max_tokens, Some(65536));
    assert_eq!(
        models[0].input,
        vec![ModelInputModality::Text, ModelInputModality::Image]
    );
    assert_eq!(models[1].base_url, "http://localhost:8080/v1");
}

#[tokio::test]
async fn discovers_chat_template_thinking_support_for_loaded_models() {
    let props_requests = Arc::new(Mutex::new(0usize));
    let server = {
        let props_requests = Arc::clone(&props_requests);
        listen(move |request: TestRequest, response| {
            let props_requests = Arc::clone(&props_requests);
            async move {
                if request.pathname() == "/models" {
                    response
                        .send_json(&json!({
                            "data": [{ "id": "qwen", "status": { "value": "loaded" }, "meta": { "n_ctx": 32768 } }]
                        }))
                        .await;
                    return;
                }
                if request.pathname() == "/props" {
                    *props_requests.lock().unwrap() += 1;
                    assert_eq!(request.query_param("model").as_deref(), Some("qwen"));
                    assert_eq!(request.query_param("autoload").as_deref(), Some("false"));
                    response
                        .send_json(&json!({ "chat_template": "{% if enable_thinking %}think{% endif %}" }))
                        .await;
                    return;
                }
                response.send_not_found().await;
            }
        })
        .await
    };

    let controller = create_llama_provider();
    controller
        .provider
        .refresh_models(refresh_context(
            Some(api_key_credential(server.origin())),
            None,
            publish_ok(),
            true,
        ))
        .await
        .expect("refresh");

    assert_eq!(*props_requests.lock().unwrap(), 1);
    let models = controller.provider.get_models();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "qwen");
    assert_eq!(models[0].reasoning, Some(true));
    let thinking = models[0].thinking_level_map.as_ref().expect("thinking map");
    assert_eq!(
        thinking.get(&kiana_ai::types::ModelThinkingLevel::Off),
        Some(&Some("off".to_string()))
    );
    assert_eq!(
        thinking.get(&kiana_ai::types::ModelThinkingLevel::Minimal),
        Some(&None)
    );
    assert_eq!(
        thinking.get(&kiana_ai::types::ModelThinkingLevel::Low),
        Some(&None)
    );
    assert_eq!(
        thinking.get(&kiana_ai::types::ModelThinkingLevel::Medium),
        Some(&Some("medium".to_string()))
    );
    assert_eq!(
        thinking.get(&kiana_ai::types::ModelThinkingLevel::High),
        Some(&None)
    );
    assert_eq!(
        thinking.get(&kiana_ai::types::ModelThinkingLevel::Xhigh),
        Some(&None)
    );
    assert_eq!(
        models[0]
            .compat
            .as_ref()
            .and_then(|compat| compat.get("thinkingFormat"))
            .and_then(Value::as_str),
        Some("qwen-chat-template")
    );
}

#[tokio::test]
async fn persists_and_restores_selectable_models_for_cache_only_startup_refreshes() {
    let server = listen(|request: TestRequest, response| async move {
        if request.pathname() == "/models" {
            response
                .send_json(&json!({
                    "data": [
                        { "id": "loaded", "status": { "value": "loaded" }, "meta": { "n_ctx": 32768 } },
                        { "id": "sleeping", "status": { "value": "sleeping" }, "meta": { "n_ctx": 32768 } },
                        { "id": "unloaded", "status": { "value": "unloaded" } }
                    ]
                }))
                .await;
            return;
        }
        if request.pathname() == "/props" && request.query_param("model").as_deref() == Some("loaded") {
            response.send_json(&json!({})).await;
            return;
        }
        response.send_not_found().await;
    })
    .await;

    let cached = llama_support::StoredEntry::default();
    let first = create_llama_provider();
    first
        .provider
        .refresh_models(refresh_context(
            Some(api_key_credential(server.origin())),
            None,
            publish_into(Arc::clone(&cached)),
            true,
        ))
        .await
        .expect("first refresh");
    assert_eq!(
        first
            .provider
            .get_models()
            .iter()
            .map(|model| model.id.clone())
            .collect::<Vec<_>>(),
        vec!["loaded".to_string(), "sleeping".to_string()]
    );
    let entry = cached.lock().unwrap().clone().expect("cached entry");
    let persisted: Vec<(String, String)> = entry
        .models
        .iter()
        .map(|model| (model.model().id.clone(), model.model().api.clone()))
        .collect();
    assert_eq!(
        persisted,
        vec![
            ("loaded".to_string(), "openai-completions".to_string()),
            ("sleeping".to_string(), "openai-completions".to_string()),
            ("loaded".to_string(), "llama-cpp-classify".to_string()),
            ("sleeping".to_string(), "llama-cpp-classify".to_string()),
        ]
    );

    let second = create_llama_provider();
    second
        .provider
        .refresh_models(refresh_context(
            Some(api_key_credential(server.origin())),
            Some(entry),
            publish_into(Arc::clone(&cached)),
            false,
        ))
        .await
        .expect("second refresh");
    let models = second.provider.get_models();
    assert_eq!(models.len(), 2);
    for model in &models {
        assert_eq!(model.base_url, format!("{}/v1", server.origin()));
        assert_eq!(model.context_window, Some(32768));
    }
    let classifiers: Vec<(String, String, String, Option<u64>)> = second
        .provider
        .get_all_models()
        .into_iter()
        .filter_map(|model| match model {
            AnyModel::Classifier(model) => {
                Some((model.id, model.api, model.base_url, model.context_window))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        classifiers,
        vec![
            (
                "loaded".to_string(),
                "llama-cpp-classify".to_string(),
                server.origin().to_string(),
                Some(32768),
            ),
            (
                "sleeping".to_string(),
                "llama-cpp-classify".to_string(),
                server.origin().to_string(),
                Some(32768),
            ),
        ]
    );
}

#[tokio::test]
async fn preserves_cached_llama_cpp_context_for_unloaded_autoload_presets() {
    let loaded = Arc::new(Mutex::new(true));
    let unloaded_args = Arc::new(Mutex::new(None::<Vec<Value>>));
    let server = {
        let loaded = Arc::clone(&loaded);
        let unloaded_args = Arc::clone(&unloaded_args);
        listen(move |request: TestRequest, response| {
            let loaded = Arc::clone(&loaded);
            let unloaded_args = Arc::clone(&unloaded_args);
            async move {
                if request.pathname() == "/models" {
                    let is_loaded = *loaded.lock().unwrap();
                    let model = if is_loaded {
                        json!({
                            "id": "qwen",
                            "status": { "value": "loaded" },
                            "source": "preset",
                            "meta": { "n_ctx": 65536, "n_ctx_train": 128000 }
                        })
                    } else {
                        let mut status = json!({ "value": "unloaded" });
                        if let Some(args) = unloaded_args.lock().unwrap().clone() {
                            status["args"] = Value::Array(args);
                        }
                        json!({
                            "id": "qwen",
                            "status": status,
                            "source": "preset",
                            "meta": { "n_ctx_train": 128000 }
                        })
                    };
                    response.send_json(&json!({ "data": [model] })).await;
                    return;
                }
                if request.pathname() == "/props"
                    && request.query_param("model").as_deref() == Some("qwen")
                {
                    response.send_json(&json!({})).await;
                    return;
                }
                if request.pathname() == "/props" {
                    response
                        .send_json(&json!({ "role": "router", "models_autoload": true }))
                        .await;
                    return;
                }
                response.send_not_found().await;
            }
        })
        .await
    };

    let cached = llama_support::StoredEntry::default();
    let stored_windows = {
        let cached = Arc::clone(&cached);
        move || -> Vec<Option<u64>> {
            cached
                .lock()
                .unwrap()
                .as_ref()
                .map(|entry| {
                    entry
                        .models
                        .iter()
                        .map(|model| model.model().context_window)
                        .collect()
                })
                .unwrap_or_default()
        }
    };
    let credential = api_key_credential(server.origin());

    let first = create_llama_provider();
    first
        .provider
        .refresh_models(refresh_context(
            Some(credential.clone()),
            None,
            publish_into(Arc::clone(&cached)),
            true,
        ))
        .await
        .expect("first refresh");
    assert_eq!(stored_windows(), vec![Some(65536), Some(65536)]);

    *loaded.lock().unwrap() = false;
    let stored = cached.lock().unwrap().clone();
    let second = create_llama_provider();
    second
        .provider
        .refresh_models(refresh_context(
            Some(credential.clone()),
            stored,
            publish_into(Arc::clone(&cached)),
            true,
        ))
        .await
        .expect("second refresh");
    let models = second.provider.get_models();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].context_window, Some(65536));
    assert_eq!(stored_windows(), vec![Some(65536), Some(65536)]);

    *unloaded_args.lock().unwrap() = Some(vec![
        json!("llama-server"),
        json!("--ctx-size"),
        json!("32768"),
    ]);
    let stored = cached.lock().unwrap().clone();
    second
        .provider
        .refresh_models(refresh_context(
            Some(credential),
            stored,
            publish_into(Arc::clone(&cached)),
            true,
        ))
        .await
        .expect("third refresh");
    assert_eq!(stored_windows(), vec![Some(32768), Some(32768)]);
}

#[tokio::test]
async fn exposes_unloaded_presets_only_when_router_autoload_is_enabled() {
    let props_requests = Arc::new(Mutex::new(0usize));
    let server = {
        let props_requests = Arc::clone(&props_requests);
        listen(move |request: TestRequest, response| {
            let props_requests = Arc::clone(&props_requests);
            async move {
                assert_eq!(request.header("authorization").as_deref(), Some("Bearer local"));
                if request.pathname() == "/models" {
                    response
                        .send_json(&json!({
                            "data": [
                                { "id": "preset", "status": { "value": "unloaded" }, "source": "preset", "meta": { "n_ctx": 65536 } },
                                { "id": "failed-preset", "status": { "value": "unloaded", "failed": true }, "source": "preset" },
                                { "id": "cache", "status": { "value": "unloaded" }, "source": "cache" },
                                { "id": "models-dir", "status": { "value": "unloaded" }, "source": "models_dir" }
                            ]
                        }))
                        .await;
                    return;
                }
                if request.pathname() == "/props" {
                    *props_requests.lock().unwrap() += 1;
                    response
                        .send_json(&json!({ "role": "router", "models_autoload": true }))
                        .await;
                    return;
                }
                response.send_not_found().await;
            }
        })
        .await
    };

    let cached = llama_support::StoredEntry::default();
    let controller = create_llama_provider();
    controller
        .provider
        .refresh_models(refresh_context(
            Some(api_key_credential(server.origin())),
            None,
            publish_into(Arc::clone(&cached)),
            true,
        ))
        .await
        .expect("refresh");
    assert_eq!(*props_requests.lock().unwrap(), 1);
    assert_eq!(
        controller
            .provider
            .get_models()
            .iter()
            .map(|model| model.id.clone())
            .collect::<Vec<_>>(),
        vec!["preset".to_string()]
    );
    let entry = cached.lock().unwrap().clone().expect("cached entry");
    let persisted: Vec<(String, String)> = entry
        .models
        .iter()
        .map(|model| (model.model().id.clone(), model.model().api.clone()))
        .collect();
    assert_eq!(
        persisted,
        vec![
            ("preset".to_string(), "openai-completions".to_string()),
            ("preset".to_string(), "llama-cpp-classify".to_string()),
        ]
    );
}

#[tokio::test]
async fn classifies_with_selectable_models_through_llama_server() {
    let paths = Arc::new(Mutex::new(Vec::<String>::new()));
    let server = {
        let paths = Arc::clone(&paths);
        listen(move |request: TestRequest, response| {
            let paths = Arc::clone(&paths);
            async move {
                paths.lock().unwrap().push(request.path.clone());
                let payload = request.json().unwrap_or(Value::Null);
                assert_eq!(payload.get("model").and_then(Value::as_str), Some("qwen"));
                match request.pathname() {
                    "/tokenize" => {
                        let content = payload.get("content").and_then(Value::as_str).unwrap_or("");
                        let tokens: Vec<u32> = content.chars().map(|c| c as u32).collect();
                        response.send_json(&json!({ "tokens": tokens })).await;
                    }
                    "/apply-template" => {
                        response
                            .send_json(&json!({ "prompt": "<|im_start|>assistant\n" }))
                            .await;
                    }
                    "/completion" => {
                        response
                            .send_json(&json!({
                                "completion_probabilities": [{
                                    "top_logprobs": [
                                        { "id": 66, "token": "B", "logprob": -0.1 },
                                        { "id": 65, "token": "A", "logprob": -2.4 }
                                    ]
                                }]
                            }))
                            .await;
                    }
                    _ => response.send_not_found().await,
                }
            }
        })
        .await
    };

    let controller = create_llama_provider();
    controller.set_catalog(&[model_info("qwen", "loaded")], server.origin(), false);
    let mut classifier = classifier_model(&controller);
    classifier.base_url = format!("{}/v1", server.origin());
    let mut criteria = IndexMap::new();
    criteria.insert("billing".to_string(), String::new());
    criteria.insert("ci".to_string(), String::new());
    let mut questions = IndexMap::new();
    questions.insert(
        "kind".to_string(),
        ClassifierQuestion::Choice {
            instructions: "What is this about?".to_string(),
            criteria,
        },
    );
    let context = ClassifierContext {
        state: json_object(json!({ "message": "The build is red again." })),
        images: None,
        questions,
    };
    let options = kiana_ai::models::ClassifierOptions {
        api_key: Some("local".to_string()),
        ..Default::default()
    };
    let result = controller
        .provider
        .classify(&classifier, &context, Some(&options))
        .await;
    assert_eq!(result.error_message, None);
    assert!(matches!(
        result.answers.get("kind"),
        Some(kiana_ai::types::ClassifierAnswer::Choice { choice, .. }) if choice == "ci"
    ));
    assert!(
        paths
            .lock()
            .unwrap()
            .iter()
            .any(|path| path == "/completion")
    );
}

#[tokio::test]
async fn exposes_decision_models_reported_by_the_catalog_as_native_system_one_classifiers() {
    let props_models = Arc::new(Mutex::new(Vec::<String>::new()));
    let system_one_requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let server = {
        let props_models = Arc::clone(&props_models);
        let system_one_requests = Arc::clone(&system_one_requests);
        listen(move |request: TestRequest, response| {
            let props_models = Arc::clone(&props_models);
            let system_one_requests = Arc::clone(&system_one_requests);
            async move {
                if request.pathname() == "/models" {
                    response
                        .send_json(&json!({
                            "data": [
                                {
                                    "id": "qwen",
                                    "status": { "value": "loaded" },
                                    "architecture": { "input_modalities": ["text"], "output_modalities": ["text"] },
                                    "meta": { "n_ctx": 32768 }
                                },
                                {
                                    "id": "kev",
                                    "status": { "value": "loaded" },
                                    "architecture": { "input_modalities": ["text"], "output_modalities": ["decisions"] },
                                    "meta": { "n_ctx": 8192 }
                                },
                                {
                                    "id": "laya",
                                    "status": { "value": "sleeping" },
                                    "architecture": { "input_modalities": ["text"], "output_modalities": ["decisions"] }
                                },
                                // 0.6.0 之前的服务器可能不带 architecture。
                                { "id": "legacy", "status": { "value": "loaded" } }
                            ]
                        }))
                        .await;
                    return;
                }
                if request.pathname() == "/props" {
                    props_models
                        .lock()
                        .unwrap()
                        .push(request.query_param("model").unwrap_or_default());
                    response.send_json(&json!({})).await;
                    return;
                }
                if request.pathname() == "/v1/systemone" {
                    let payload = request.json().unwrap_or(Value::Null);
                    system_one_requests.lock().unwrap().push(payload.clone());
                    let model = payload
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let questions = payload
                        .get("questions")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    let answers: serde_json::Map<String, Value> = questions
                        .keys()
                        .map(|id| (id.clone(), json!({ "type": "noul", "noul": 0.82 })))
                        .collect();
                    response
                        .send_json(&json!({
                            "model": model,
                            "answers": answers,
                            "usage": { "input_tokens": 42, "output_tokens": 0 }
                        }))
                        .await;
                    return;
                }
                response.send_not_found().await;
            }
        })
        .await
    };

    let cached = llama_support::StoredEntry::default();
    let credential = api_key_credential(server.origin());
    let controller = create_llama_provider();
    controller
        .provider
        .refresh_models(refresh_context(
            Some(credential.clone()),
            None,
            publish_into(Arc::clone(&cached)),
            true,
        ))
        .await
        .expect("refresh");

    // 目录已识别 decision 模型：刷新既不探测它们、也不读取它们的 chat template。
    assert!(system_one_requests.lock().unwrap().is_empty());
    let mut props = props_models.lock().unwrap().clone();
    props.sort();
    assert_eq!(props, vec!["legacy".to_string(), "qwen".to_string()]);
    assert_eq!(
        controller
            .provider
            .get_models()
            .iter()
            .map(|model| model.id.clone())
            .collect::<Vec<_>>(),
        vec!["qwen".to_string(), "legacy".to_string()]
    );
    let entry = cached.lock().unwrap().clone().expect("entry");
    let persisted: Vec<(String, String, String)> = entry
        .models
        .iter()
        .map(|model| {
            (
                model.model().id.clone(),
                model.model().api.clone(),
                model.model().base_url.clone(),
            )
        })
        .collect();
    assert_eq!(
        persisted,
        vec![
            (
                "qwen".to_string(),
                "openai-completions".to_string(),
                format!("{}/v1", server.origin())
            ),
            (
                "legacy".to_string(),
                "openai-completions".to_string(),
                format!("{}/v1", server.origin())
            ),
            (
                "qwen".to_string(),
                "llama-cpp-classify".to_string(),
                server.origin().to_string()
            ),
            (
                "kev".to_string(),
                "typesafe-system-one".to_string(),
                format!("{}/v1", server.origin())
            ),
            (
                "laya".to_string(),
                "typesafe-system-one".to_string(),
                format!("{}/v1", server.origin())
            ),
            (
                "legacy".to_string(),
                "llama-cpp-classify".to_string(),
                server.origin().to_string()
            ),
        ]
    );

    let kev = controller
        .provider
        .get_all_models()
        .into_iter()
        .find_map(|model| match model {
            AnyModel::Classifier(model) if model.id == "kev" => Some(model),
            _ => None,
        })
        .expect("decision classifier");
    assert_eq!(kev.context_window, Some(8192));
    let mut questions = IndexMap::new();
    questions.insert(
        "angry".to_string(),
        ClassifierQuestion::Bool {
            instructions: "Is the customer angry?".to_string(),
            criteria: kiana_ai::types::BoolQuestionCriteria {
                r#true: "angry".to_string(),
                r#false: "calm".to_string(),
            },
        },
    );
    let context = ClassifierContext {
        state: json_object(json!({ "message": "I was charged twice." })),
        images: None,
        questions,
    };
    let options = kiana_ai::models::ClassifierOptions {
        api_key: Some("local".to_string()),
        ..Default::default()
    };
    let result = controller
        .provider
        .classify(&kev, &context, Some(&options))
        .await;
    assert_eq!(result.error_message, None);
    assert_eq!(
        result.answers.get("angry"),
        Some(&kiana_ai::types::ClassifierAnswer::Bool { probability: 0.82 })
    );
    let usage = result.usage.expect("usage");
    assert_eq!((usage.input, usage.output), (42, 0));
    let requests = system_one_requests.lock().unwrap().clone();
    // TS 断言完整请求体（含 instructions/criteria），不是只检查 model/state/type。
    assert_eq!(
        requests,
        vec![json!({
            "model": "kev",
            "state": { "message": "I was charged twice." },
            "questions": {
                "angry": {
                    "type": "noul",
                    "instructions": "Is the customer angry?",
                    "criteria": { "true": "angry", "false": "calm" }
                }
            }
        })]
    );

    // 仅缓存的启动恢复 decision 模型及其原生 API。
    let restored = create_llama_provider();
    restored
        .provider
        .refresh_models(refresh_context(
            Some(credential),
            Some(entry),
            publish_into(Arc::clone(&cached)),
            false,
        ))
        .await
        .expect("restore");
    assert_eq!(
        restored
            .provider
            .get_models()
            .iter()
            .map(|model| model.id.clone())
            .collect::<Vec<_>>(),
        vec!["qwen".to_string(), "legacy".to_string()]
    );
    let restored_classifiers: Vec<(String, String)> = restored
        .provider
        .get_all_models()
        .into_iter()
        .filter_map(|model| match model {
            AnyModel::Classifier(model) => Some((model.id, model.api)),
            _ => None,
        })
        .collect();
    assert_eq!(
        restored_classifiers,
        vec![
            ("qwen".to_string(), "llama-cpp-classify".to_string()),
            ("kev".to_string(), "typesafe-system-one".to_string()),
            ("laya".to_string(), "typesafe-system-one".to_string()),
            ("legacy".to_string(), "llama-cpp-classify".to_string()),
        ]
    );
}

#[test]
fn lists_decision_models_that_also_output_text_for_chat() {
    let controller = create_llama_provider();
    let mut decide = model_info("decide", "sleeping");
    decide.architecture = Some(LlamaModelArchitecture {
        input_modalities: Vec::new(),
        output_modalities: vec!["decisions".to_string()],
    });
    let mut hybrid = model_info("hybrid", "loaded");
    hybrid.architecture = Some(LlamaModelArchitecture {
        input_modalities: Vec::new(),
        output_modalities: vec!["text".to_string(), "decisions".to_string()],
    });
    controller.set_catalog(&[decide, hybrid], "http://localhost:8080", false);
    assert_eq!(
        controller
            .provider
            .get_models()
            .iter()
            .map(|model| model.id.clone())
            .collect::<Vec<_>>(),
        vec!["hybrid".to_string()]
    );
    let classifiers: Vec<(String, String)> = controller
        .provider
        .get_all_models()
        .into_iter()
        .filter_map(|model| match model {
            AnyModel::Classifier(model) => Some((model.id, model.api)),
            _ => None,
        })
        .collect();
    assert_eq!(
        classifiers,
        vec![
            ("decide".to_string(), "typesafe-system-one".to_string()),
            ("hybrid".to_string(), "typesafe-system-one".to_string()),
        ]
    );
}

#[tokio::test]
async fn hides_unloaded_presets_when_router_autoload_is_disabled() {
    // 纯 JSON 端点用 wiremock（有状态/SSE 端点用 `llama_support` 的真实 TcpListener）。
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(kiana_json::js_object::stringify(&json!({
                    "data": [{ "id": "preset", "status": { "value": "unloaded" }, "source": "preset" }]
                }))),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/props"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(kiana_json::js_object::stringify(&json!({
                    "role": "router", "models_autoload": false
                }))),
        )
        .mount(&server)
        .await;

    let controller = create_llama_provider();
    controller
        .provider
        .refresh_models(refresh_context(
            Some(api_key_credential(&server.uri())),
            None,
            publish_ok(),
            true,
        ))
        .await
        .expect("refresh");
    assert!(controller.provider.get_models().is_empty());
}

#[tokio::test]
async fn stays_dormant_until_configured_and_stores_url_plus_optional_key() {
    let controller = create_llama_provider();
    let auth = controller
        .provider
        .auth()
        .api_key
        .as_ref()
        .expect("api key auth")
        .clone();
    let empty_context = EmptyAuthContext;
    let signal = AbortScope::new();
    let check = auth
        .check(ApiKeyAuthInput {
            ctx: &empty_context,
            credential: None,
            signal: &signal,
        })
        .await
        .expect("check");
    assert!(check.is_none());
    let resolved = auth
        .resolve(ApiKeyAuthInput {
            ctx: &empty_context,
            credential: None,
            signal: &signal,
        })
        .await
        .expect("resolve");
    assert!(resolved.is_none());

    let server = listen(|request: TestRequest, response| async move {
        assert_eq!(
            request.header("authorization").as_deref(),
            Some("Bearer secret")
        );
        response.send_json(&json!({ "data": [] })).await;
    })
    .await;

    let interaction = FakeInteraction::new(
        AbortScope::new(),
        vec![server.origin().to_string(), "secret".to_string()],
    );
    let credential = auth.login(&interaction).await.expect("login");
    let mut expected_env = IndexMap::new();
    expected_env.insert("LLAMA_BASE_URL".to_string(), server.origin().to_string());
    assert_eq!(
        credential,
        ApiKeyCredential {
            key: Some("secret".to_string()),
            env: Some(expected_env.clone()),
            extra: IndexMap::new(),
        }
    );
    let resolved = auth
        .resolve(ApiKeyAuthInput {
            ctx: &empty_context,
            credential: Some(&credential),
            signal: &signal,
        })
        .await
        .expect("resolve")
        .expect("auth result");
    assert_eq!(
        resolved,
        AuthResult {
            auth: ModelAuth {
                api_key: Some("secret".to_string()),
                headers: None,
                base_url: Some(format!("{}/v1", server.origin())),
            },
            env: Some(expected_env),
            source: Some("stored credential".to_string()),
        }
    );
    let check = auth
        .check(ApiKeyAuthInput {
            ctx: &empty_context,
            credential: Some(&credential),
            signal: &signal,
        })
        .await
        .expect("check")
        .expect("check result");
    assert_eq!(check.source.as_deref(), Some("stored credential"));
}

#[tokio::test]
async fn searches_hugging_face_and_reads_quantizations_plus_access_requirements() {
    let server = listen(|request: TestRequest, response| async move {
        assert_eq!(
            request.header("authorization").as_deref(),
            Some("Bearer hf-secret")
        );
        if request.pathname() == "/api/models" {
            assert_eq!(request.query_param("search").as_deref(), Some("qwen coder"));
            assert_eq!(request.query_param("filter").as_deref(), Some("gguf"));
            assert_eq!(request.query_param("sort").as_deref(), Some("downloads"));
            response
                .send_json(&json!([{ "id": "owner/model-GGUF", "downloads": 1200 }]))
                .await;
            return;
        }
        if request.path == "/api/models/owner/model-GGUF?blobs=true" {
            response
                .send_json(&json!({
                    "id": "owner/model-GGUF",
                    "gated": "manual",
                    "siblings": [
                        { "rfilename": "model-Q5_K_M.gguf", "size": 6000 },
                        { "rfilename": "model-Q4_K_M-00001-of-00002.gguf", "size": 2000 },
                        { "rfilename": "model-Q4_K_M-00002-of-00002.gguf", "size": 3000 },
                        { "rfilename": "mmproj-F16.gguf", "size": 1000 }
                    ]
                }))
                .await;
            return;
        }
        response.send_not_found().await;
    })
    .await;
    let client = HuggingFaceClient::new(Some("hf-secret".to_string()), Some(server.origin()));
    assert_eq!(
        client.search("qwen coder", None).await.expect("search"),
        vec![HuggingFaceModel {
            id: "owner/model-GGUF".to_string(),
            downloads: 1200.0,
        }]
    );
    assert_eq!(
        client
            .details("owner/model-GGUF", None)
            .await
            .expect("details"),
        kiana_builtin_extensions::llama::huggingface::HuggingFaceModelDetails {
            id: "owner/model-GGUF".to_string(),
            gated: HuggingFaceGated::Manual,
            quantizations: vec![
                HuggingFaceQuantization {
                    name: "Q4_K_M".to_string(),
                    size: Some(5000.0),
                },
                HuggingFaceQuantization {
                    name: "Q5_K_M".to_string(),
                    size: Some(6000.0),
                },
            ],
        }
    );
    let mut env = IndexMap::new();
    env.insert("HF_TOKEN".to_string(), " hf-secret ".to_string());
    assert_eq!(
        find_huggingface_token(Some(&env)).await.as_deref(),
        Some("hf-secret")
    );
}

#[tokio::test]
async fn loads_with_sse_progress_and_waits_for_the_loaded_catalog_state() {
    let status = Arc::new(Mutex::new("unloaded".to_string()));
    let streams = SseStreams::default();
    let server = {
        let status = Arc::clone(&status);
        let streams = streams.clone();
        listen(move |request: TestRequest, response| {
            let status = Arc::clone(&status);
            let streams = streams.clone();
            async move {
                if request.pathname() == "/models/sse" {
                    response.stream_head(200, "text/event-stream").await;
                    streams.add(response);
                    std::future::pending::<()>().await;
                    return;
                }
                if request.pathname() == "/models/load" && request.method == "POST" {
                    *status.lock().unwrap() = "loading".to_string();
                    response.send_json(&json!({ "success": true })).await;
                    tokio::spawn(async move {
                        wait_ms(20).await;
                        streams
                            .send(&json!({
                                "model": "test-model",
                                "event": "status_change",
                                "data": {
                                    "status": "loading",
                                    "progress": {
                                        "stages": ["text_model", "mmproj_model"],
                                        "current": "text_model",
                                        "value": 0.5
                                    }
                                }
                            }))
                            .await;
                        *status.lock().unwrap() = "loaded".to_string();
                        streams
                            .send(&json!({
                                "model": "test-model",
                                "event": "status_change",
                                "data": { "status": "loaded" }
                            }))
                            .await;
                    });
                    return;
                }
                if request.pathname() == "/models" {
                    response
                        .send_json(&json!({
                            "data": [{ "id": "test-model", "status": { "value": *status.lock().unwrap() } }]
                        }))
                        .await;
                    return;
                }
                response.send_not_found().await;
            }
        })
        .await
    };

    let progress = Arc::new(Mutex::new(Vec::<String>::new()));
    let progress_for_callback = Arc::clone(&progress);
    let client = LlamaClient::new(server.origin(), None).expect("client");
    let model = client
        .load_and_wait(
            "test-model",
            move |entry| progress_for_callback.lock().unwrap().push(entry.message),
            None,
        )
        .await
        .expect("load");
    assert_eq!(model.status.value, "loaded");
    assert!(
        progress
            .lock()
            .unwrap()
            .iter()
            .any(|message| message == "Loading text model")
    );
}

/// `status.failed` 按 JS 真值（`failed: 1` 也算失败）；`exit_code` 用 `=== undefined`
/// 判定——键存在但为 `null` 时消息是 `Model exited with code null`，不能回落到
/// `Model failed to load`（旧实现只接受 bool/数字，会一直轮询到超时）。
#[tokio::test]
async fn load_failure_uses_js_truthiness_and_undefined_checks() {
    let server = listen(|request: TestRequest, response| async move {
        if request.pathname() == "/models/sse" {
            response.stream_head(200, "text/event-stream").await;
            std::future::pending::<()>().await;
            return;
        }
        if request.pathname() == "/models/load" && request.method == "POST" {
            response.send_json(&json!({ "success": true })).await;
            return;
        }
        if request.pathname() == "/models" {
            response
                .send_json(&json!({
                    "data": [{
                        "id": "bad",
                        "status": { "value": "unloaded", "failed": 1, "exit_code": null }
                    }]
                }))
                .await;
            return;
        }
        response.send_not_found().await;
    })
    .await;

    let client = LlamaClient::new(server.origin(), None).expect("client");
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        client.load_and_wait("bad", |_| {}, None),
    )
    .await
    .expect("load must fail instead of polling forever")
    .expect_err("load failure");
    assert_eq!(error.message, "Model exited with code null");
}

/// 直接驱动 `LlamaClient::watch`：跨 chunk 的半帧、跨 chunk 的 UTF-8 多字节字符、多行
/// `data:`、`\r\n` 行尾与静默丢弃（spec §2.16.10 / §8.4 #9）。TS 测试没有覆盖这些细节，
/// 这里用真实 chunked 响应锁定。
#[tokio::test]
async fn buffers_partial_sse_frames_and_multiline_data() {
    let server = listen(|request: TestRequest, response| async move {
        if request.pathname() == "/models/sse" {
            response.stream_head(200, "text/event-stream").await;
            // 1) 帧被切成两段，且第二段从 UTF-8 字符的中间开始（✓ = E2 9C 93）。
            response.stream_bytes(b"data: {\"model\":\"test\xe2").await;
            response
                .stream_bytes(b"\x9c\x93\",\"event\":\"model_status\",\"data\":{\"status\":\"lo")
                .await;
            response.stream_bytes(b"aded\"}}\n\n").await;
            // 2) 多行 data: 用 \n 连接后才是合法 JSON。
            response
                .stream_chunk(
                    "data: {\"model\":\"m\",\ndata: \"event\":\"status_change\",\"data\":{}}\n\n",
                )
                .await;
            // 3) 非法 JSON 与缺字段的帧被静默丢弃。
            response.stream_chunk("data: not json\n\n").await;
            response.stream_chunk("data: {\"model\":\"m\"}\n\n").await;
            // 4) \r\n 行尾在 chunk 内归一。
            response
                .stream_chunk("data: {\"model\":\"m\",\"event\":\"x\"}\r\n\r\n")
                .await;
            response.stream_end().await;
            return;
        }
        response.send_not_found().await;
    })
    .await;

    let events = Arc::new(Mutex::new(Vec::new()));
    let events_for_callback = Arc::clone(&events);
    let client = LlamaClient::new(server.origin(), None).expect("client");
    client
        .watch(
            move |event| events_for_callback.lock().unwrap().push(event),
            None,
        )
        .await
        .expect("watch");
    let events = events.lock().unwrap().clone();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].model, "test✓");
    assert_eq!(events[0].event, "model_status");
    assert_eq!(
        events[0].data.as_ref().and_then(|data| data.get("status")),
        Some(&json!("loaded"))
    );
    assert_eq!(events[1].model, "m");
    assert_eq!(events[1].event, "status_change");
    assert_eq!(events[2].model, "m");
    assert_eq!(events[2].event, "x");
}

#[tokio::test]
async fn downloads_with_byte_progress_and_returns_the_refreshed_catalog() {
    let status = Arc::new(Mutex::new("missing".to_string()));
    let streams = SseStreams::default();
    let server = {
        let status = Arc::clone(&status);
        let streams = streams.clone();
        listen(move |request: TestRequest, response| {
            let status = Arc::clone(&status);
            let streams = streams.clone();
            async move {
                if request.pathname() == "/models/sse" {
                    response.stream_head(200, "text/event-stream").await;
                    streams.add(response);
                    std::future::pending::<()>().await;
                    return;
                }
                if request.pathname() == "/models" && request.method == "POST" {
                    *status.lock().unwrap() = "downloading".to_string();
                    response.send_json(&json!({ "success": true })).await;
                    tokio::spawn(async move {
                        wait_ms(20).await;
                        streams
                            .send(&json!({
                                "model": "owner/repo:Q4_K_M",
                                "event": "download_progress",
                                "data": {
                                    "progress": {
                                        "https://example/model.gguf": { "done": 512, "total": 1024 }
                                    }
                                }
                            }))
                            .await;
                        *status.lock().unwrap() = "unloaded".to_string();
                        streams
                            .send(&json!({
                                "model": "owner/repo:Q4_K_M",
                                "event": "download_finished",
                                "data": {}
                            }))
                            .await;
                    });
                    return;
                }
                if request.pathname() == "/models" {
                    let value = status.lock().unwrap().clone();
                    let data = if value == "missing" {
                        json!([])
                    } else {
                        json!([{ "id": "owner/repo:Q4_K_M", "status": { "value": value } }])
                    };
                    response.send_json(&json!({ "data": data })).await;
                    return;
                }
                response.send_not_found().await;
            }
        })
        .await
    };

    let progress = Arc::new(Mutex::new(Vec::new()));
    let progress_for_callback = Arc::clone(&progress);
    let client = LlamaClient::new(server.origin(), None).expect("client");
    let models = client
        .download_and_wait(
            "owner/repo:Q4_K_M",
            move |entry| progress_for_callback.lock().unwrap().push(entry),
            None,
        )
        .await
        .expect("download");
    assert_eq!(models, vec![model_info("owner/repo:Q4_K_M", "unloaded")]);
    assert!(progress.lock().unwrap().contains(
        &kiana_builtin_extensions::llama::client::LlamaProgress {
            message: "Downloading model".to_string(),
            ratio: Some(Some(0.5)),
            detail: Some(Some("512 B / 1.00 KiB".to_string())),
        }
    ));
}

/// `downloadAndWait` 在 `await list()` **之后**读取 `finished`/`sawDownloading`：SSE 的
/// `download_finished` 在目录请求飞行期间到达时，当前迭代就应 reload 返回，而不是再睡
/// 500 ms 多轮询一次（旧实现把快照取在 await 之前）。用普通 GET 次数断言，避免计时抖动。
#[tokio::test]
async fn download_finished_during_catalog_poll_returns_without_an_extra_poll() {
    let streams = SseStreams::default();
    let server = {
        let streams = streams.clone();
        listen(move |request: TestRequest, response| {
            let streams = streams.clone();
            async move {
                if request.pathname() == "/models/sse" {
                    response.stream_head(200, "text/event-stream").await;
                    streams.add(response);
                    std::future::pending::<()>().await;
                    return;
                }
                if request.pathname() == "/models" && request.method == "POST" {
                    response.send_json(&json!({ "success": true })).await;
                    tokio::spawn(async move {
                        wait_ms(20).await;
                        streams
                            .send(&json!({
                                "model": "owner/repo:Q4_K_M",
                                "event": "download_finished",
                                "data": {}
                            }))
                            .await;
                    });
                    return;
                }
                if request.pathname() == "/models" {
                    if request.query_param("reload").as_deref() == Some("1") {
                        response.send_json(&json!({ "data": [] })).await;
                        return;
                    }
                    // 目录请求比 SSE 事件慢：事件必须在本次迭代被观察到。
                    wait_ms(100).await;
                    response
                        .send_json(&json!({
                            "data": [{
                                "id": "owner/repo:Q4_K_M",
                                "status": { "value": "unloaded" }
                            }]
                        }))
                        .await;
                    return;
                }
                response.send_not_found().await;
            }
        })
        .await
    };

    let client = LlamaClient::new(server.origin(), None).expect("client");
    let models = client
        .download_and_wait("owner/repo:Q4_K_M", |_| {}, None)
        .await
        .expect("download");
    assert_eq!(models, Vec::<LlamaModelInfo>::new());
    let plain_gets = server
        .requests()
        .iter()
        .filter(|request| {
            request.pathname() == "/models"
                && request.method == "GET"
                && request.query_param("reload").is_none()
        })
        .count();
    assert_eq!(
        plain_gets, 1,
        "a finish event seen during the poll must return in the same iteration"
    );
}

/// TS 的 `if (this.token)`：空字符串是假值，Hugging Face 请求不带 `Authorization`。
#[tokio::test]
async fn empty_hugging_face_token_sends_no_authorization_header() {
    let requests = Arc::new(Mutex::new(Vec::<TestRequest>::new()));
    let recorded = Arc::clone(&requests);
    let server = listen(move |request: TestRequest, response| {
        let recorded = Arc::clone(&recorded);
        async move {
            recorded.lock().unwrap().push(request);
            response
                .send_json(&json!([{ "id": "owner/repo", "downloads": 1 }]))
                .await;
        }
    })
    .await;

    let empty = HuggingFaceClient::new(Some(String::new()), Some(server.origin()));
    empty.search("qwen", None).await.expect("search");
    let keyed = HuggingFaceClient::new(Some("secret".to_string()), Some(server.origin()));
    keyed.search("qwen", None).await.expect("search");

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].header("authorization"), None);
    assert_eq!(
        requests[1].header("authorization").as_deref(),
        Some("Bearer secret")
    );
}

/// AbortSignals 的 reason 必须穿过冻结的 `CancellationToken` 契约恢复：
/// `syncCatalog` 的 15 s `AbortSignal.timeout` 与请求自带的 15 s 超时同长，外层计时器通常
/// 先触发；TS 的 `fetch` 以 `signal.reason`（`TimeoutError`）reject，`/llama` 的连接错误
/// 识别因此显示 `Could not connect to the server.`。回归：不能退化成
/// `This operation was aborted`（§2.16.10、§2.19.1）。
#[tokio::test]
async fn aborted_requests_surface_the_signal_reason() {
    let server = listen(|request: TestRequest, response| async move {
        if request.pathname() == "/models" {
            // 永不及时响应：让调用方的 signal 先中止。
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            response.send_json(&json!({ "data": [] })).await;
            return;
        }
        response.send_not_found().await;
    })
    .await;

    let signal = AbortScope::new();
    let abort = signal.clone();
    tokio::spawn(async move {
        wait_ms(30).await;
        abort.abort_with(kiana_ai::utils::abort::AbortReason {
            name: "TimeoutError".to_string(),
            message: "The operation was aborted due to timeout".to_string(),
        });
    });

    let client = LlamaClient::new(server.origin(), None).expect("client");
    let error = client
        .list(kiana_builtin_extensions::llama::client::ListOptions {
            reload: false,
            signal: Some(&signal),
        })
        .await
        .expect_err("aborted list");
    assert_eq!(error.name, "TimeoutError");
    assert_eq!(error.message, "The operation was aborted due to timeout");
}

/// `if (apiKey)` 是假值判断：空字符串不发 `Authorization`（provider.resolve 的 `??` 会保留
/// 空的 `LLAMA_API_KEY`）。
#[tokio::test]
async fn empty_api_key_sends_no_authorization_header() {
    let requests = Arc::new(Mutex::new(Vec::<TestRequest>::new()));
    let recorded = Arc::clone(&requests);
    let server = listen(move |request: TestRequest, response| {
        let recorded = Arc::clone(&recorded);
        async move {
            recorded.lock().unwrap().push(request);
            response.send_json(&json!({ "data": [] })).await;
        }
    })
    .await;

    let empty = LlamaClient::new(server.origin(), Some(String::new())).expect("client");
    empty
        .list(kiana_builtin_extensions::llama::client::ListOptions::default())
        .await
        .expect("list");
    let keyed = LlamaClient::new(server.origin(), Some("secret".to_string())).expect("client");
    keyed
        .list(kiana_builtin_extensions::llama::client::ListOptions::default())
        .await
        .expect("list");

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].header("authorization"), None);
    assert_eq!(
        requests[1].header("authorization").as_deref(),
        Some("Bearer secret")
    );
}

/// `fetch` 对 204/205/304 返回 `body === null`；TS 的 `watch` 用
/// `if (!response.ok || !response.body)` 报 `llama.cpp SSE returned HTTP <status>`。
#[tokio::test]
async fn watch_rejects_null_body_statuses() {
    let server = listen(|request: TestRequest, response| async move {
        if request.pathname() == "/models/sse" {
            response.send(204, &[], "").await;
            return;
        }
        response.send_not_found().await;
    })
    .await;
    let client = LlamaClient::new(server.origin(), None).expect("client");
    let error = client.watch(|_| {}, None).await.expect_err("null body");
    assert_eq!(error.message, "llama.cpp SSE returned HTTP 204");
}
