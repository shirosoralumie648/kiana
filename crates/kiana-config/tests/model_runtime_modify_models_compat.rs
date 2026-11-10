//! `model-runtime-modify-models-compat.test.ts` 的移植（6 个用例）。

#[path = "fixtures/model_runtime/mod.rs"]
mod support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::auth::types::{
    AuthOperationOptions, AuthType, CredentialStore, LoginOptions, OAuthCredential,
    OAuthCredentials, ProviderAuth,
};
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::models::{ModelsRefreshOptions, ModelsStore, ModelsStoreOperationOptions, Provider};
use kiana_ai::types::{
    AssistantMessage, AssistantMessageEvent, DeferredCancelOptions, DeferredFetchOptions,
    DeferredHandle, DoneReason, Model, ModelType, ProviderHeaders, SimpleStreamOptions, StopReason,
    StreamOptions, TranscriptContext,
};
use kiana_config::auth::storage::{AuthStorage, auth_data_from_value};
use kiana_config::models::composer::{
    ExtensionGetApiKeyFn, ExtensionLoginFn, ExtensionOAuthConfig, ExtensionRefreshTokenFn,
    ProviderConfigInput, ProviderModelConfig,
};
use kiana_config::models::registry::ModelRegistry;
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use serde_json::json;
use support::*;

fn model(id: &str) -> Model {
    Model {
        provider: "extension-oauth".to_string(),
        ..chat_model("extension-oauth", id)
    }
}

/// 空 runtime（`modelsPath: null`，内存凭证与 models store）。
async fn empty_runtime(credentials: Arc<dyn CredentialStore>) -> Arc<ModelRuntime> {
    Arc::new(
        ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(credentials),
            models_path: ModelsPathOption::Disabled,
            allow_model_network: false,
            ..Default::default()
        })
        .await
        .expect("runtime"),
    )
}

fn memory_auth(auth: serde_json::Value) -> Arc<AuthStorage> {
    Arc::new(AuthStorage::in_memory(auth_data_from_value(auth)))
}

/// 原生 provider 的 OAuth auth 桩。
struct TestOAuthAuth {
    name: String,
    is_subscription: bool,
    to_api_key: bool,
}

#[async_trait]
impl kiana_ai::auth::types::OAuthAuth for TestOAuthAuth {
    fn name(&self) -> &str {
        &self.name
    }

    fn is_subscription(&self) -> bool {
        self.is_subscription
    }

    async fn login(
        &self,
        _interaction: &dyn kiana_ai::auth::types::AuthInteraction,
        _options: &LoginOptions,
    ) -> Result<OAuthCredential, kiana_ai::auth::types::AuthError> {
        Err(kiana_ai::auth::types::AuthError::new("unused"))
    }

    async fn refresh(
        &self,
        credential: &OAuthCredential,
        _signal: &kiana_ai::utils::abort::AbortScope,
    ) -> Result<OAuthCredential, kiana_ai::auth::types::AuthError> {
        Ok(credential.clone())
    }

    async fn to_auth(
        &self,
        credential: &OAuthCredential,
    ) -> Result<kiana_ai::auth::types::ModelAuth, kiana_ai::auth::types::AuthError> {
        Ok(kiana_ai::auth::types::ModelAuth {
            api_key: self.to_api_key.then(|| credential.access.clone()),
            ..Default::default()
        })
    }
}

#[tokio::test]
async fn registers_native_kiana_ai_providers_with_their_auth_implementation() {
    let runtime = empty_runtime(memory_auth(json!({})) as Arc<dyn CredentialStore>).await;
    let native_model = Model {
        provider: "extension-native".to_string(),
        base_url: "https://fallback.test/v1".to_string(),
        ..model("native")
    };
    let auth = TestApiKeyAuth::new("Native setup")
        .with_login(Arc::new(|| {
            Box::pin(async {
                Ok(kiana_ai::auth::types::ApiKeyCredential {
                    key: Some("secret".to_string()),
                    env: None,
                    extra: IndexMap::new(),
                })
            })
        }))
        .with_check(Arc::new(|credential| {
            Box::pin(async move {
                Ok(credential.and_then(|credential| {
                    credential.key.map(|_| kiana_ai::auth::types::AuthCheck {
                        source: Some("stored native key".to_string()),
                        r#type: AuthType::ApiKey,
                    })
                }))
            })
        }))
        .with_resolve(Arc::new(|credential| {
            Box::pin(async move {
                Ok(credential.and_then(|credential| {
                    credential.key.map(|key| {
                        kiana_ai::auth::types::AuthResult::new(kiana_ai::auth::types::ModelAuth {
                            api_key: Some(key),
                            base_url: Some("https://resolved.test/v1".to_string()),
                            ..Default::default()
                        })
                    })
                }))
            })
        }));
    let provider: Arc<dyn Provider> = TestProvider::new(
        "extension-native",
        Arc::new(auth).provider_auth(),
        vec![native_model],
    )
    .with_name("Extension Native")
    .into_provider();

    runtime
        .register_native_provider(Arc::clone(&provider))
        .expect("register");
    let registry = ModelRegistry::new(Arc::clone(&runtime));
    let registered = registry
        .get_provider("extension-native")
        .expect("registered provider");
    assert!(Arc::ptr_eq(&registered, &provider));
    assert!(
        registry
            .get_registered_native_provider("extension-native")
            .is_some_and(|native| Arc::ptr_eq(&native, &provider))
    );
    assert!(
        registry
            .get_registered_provider_ids()
            .contains(&"extension-native".to_string())
    );
    assert!(registry.find("extension-native", "native").is_some());

    let interaction = TestInteraction::new();
    runtime
        .login(
            "extension-native",
            AuthType::ApiKey,
            &interaction,
            LoginOptions::default(),
        )
        .await
        .expect("login");

    let auth = registry
        .get_provider_auth("extension-native")
        .await
        .expect("auth")
        .expect("configured");
    assert_eq!(auth.auth.api_key.as_deref(), Some("secret"));
    assert_eq!(
        auth.auth.base_url.as_deref(),
        Some("https://resolved.test/v1")
    );

    registry.unregister_provider("extension-native");
    assert!(registry.get_provider("extension-native").is_none());
}

#[tokio::test]
async fn marks_a_native_provider_with_a_stored_credential_as_configured_when_it_registers() {
    let runtime = empty_runtime(memory_auth(json!({
        "extension-native": {
            "type": "oauth",
            "access": "access",
            "refresh": "refresh",
            "expires": now_ms() + 3_600_000
        }
    })) as Arc<dyn CredentialStore>)
    .await;
    let native_model = Model {
        provider: "extension-native".to_string(),
        ..model("native")
    };
    let provider: Arc<dyn Provider> = TestProvider::new(
        "extension-native",
        ProviderAuth {
            api_key: None,
            oauth: Some(Arc::new(TestOAuthAuth {
                name: "Native OAuth".to_string(),
                is_subscription: false,
                to_api_key: true,
            })),
        },
        vec![native_model],
    )
    .with_name("Extension Native")
    .into_provider();

    runtime
        .register_native_provider(provider)
        .expect("register");

    assert!(runtime.has_configured_auth("extension-native"));
    assert!(runtime.is_using_oauth("extension-native"));
    assert!(
        runtime
            .get_available_snapshot()
            .iter()
            .any(|model| model.provider == "extension-native" && model.id == "native")
    );

    runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        })
        .await
        .expect("refresh");
    assert!(runtime.has_configured_auth("extension-native"));
}

fn completed_message(model: &Model) -> AssistantMessage {
    AssistantMessage {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        content: Vec::new(),
        stop_reason: StopReason::Stop,
        ..kiana_ai::providers::faux::faux_assistant_message(
            "",
            kiana_ai::providers::faux::FauxAssistantMessageOptions::default(),
        )
    }
}

fn completed_stream(message: AssistantMessage) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::new();
    stream
        .push(AssistantMessageEvent::Start {
            partial: Box::new(message.clone()),
        })
        .expect("push start");
    stream
        .push(AssistantMessageEvent::Done {
            reason: DoneReason::Stop,
            message: Box::new(message.clone()),
        })
        .expect("push done");
    stream
}

#[tokio::test]
async fn preserves_native_deferred_methods_through_provider_overlays() {
    let harness = Harness::new();
    harness.write_providers(json!({
        "extension-native-deferred": { "baseUrl": "https://overlay.test/v1" }
    }));
    let runtime = harness.create().await;

    let native_model = Model {
        provider: "extension-native-deferred".to_string(),
        base_url: "https://native.test/v1".to_string(),
        ..model("native-deferred")
    };
    // 捕获 (baseUrl, apiKey, headers, wait/需要克隆的字段)：DeferredFetchOptions 不实现 Clone。
    type Captured = (String, Option<String>, Option<ProviderHeaders>, Option<u64>);
    let fetched: Arc<Mutex<Option<Captured>>> = Arc::new(Mutex::new(None));
    let fetched_slot = Arc::clone(&fetched);
    let cancelled: Arc<Mutex<Option<Captured>>> = Arc::new(Mutex::new(None));
    let cancelled_slot = Arc::clone(&cancelled);

    let auth = TestApiKeyAuth::new("Native key").with_resolve(Arc::new(|_credential| {
        Box::pin(async {
            Ok(Some(kiana_ai::auth::types::AuthResult::new(
                kiana_ai::auth::types::ModelAuth {
                    api_key: Some("key".to_string()),
                    ..Default::default()
                },
            )))
        })
    }));
    let provider: Arc<dyn Provider> = TestProvider::new(
        "extension-native-deferred",
        Arc::new(auth).provider_auth(),
        vec![native_model.clone()],
    )
    .with_name("Extension Native Deferred")
    .with_fetch_deferred(Arc::new(move |request_model, _handle, options| {
        *fetched_slot.lock().unwrap() = Some((
            request_model.base_url.clone(),
            options.base.api_key.clone(),
            options.base.headers.clone(),
            options.wait,
        ));
        completed_stream(completed_message(request_model))
    }))
    .with_cancel_deferred(Arc::new(move |_request_model, handle, options| {
        *cancelled_slot.lock().unwrap() = Some((
            handle.id.clone(),
            options.api_key.clone(),
            options.headers.clone(),
            options.timeout_ms,
        ));
        Box::pin(async { Ok(()) })
    }))
    .into_provider();

    runtime
        .register_native_provider(provider)
        .expect("register");
    let composed = runtime
        .get_model("extension-native-deferred", "native-deferred")
        .expect("composed model");

    let handle = |id: &str| DeferredHandle {
        provider: "extension-native-deferred".to_string(),
        model_id: "native-deferred".to_string(),
        api: native_model.api.clone(),
        id: id.to_string(),
        expires_at: None,
        poll_after_ms: None,
        data: None,
    };
    let transform: kiana_ai::models::TransformHeadersFn =
        Arc::new(|mut headers: ProviderHeaders| {
            headers.insert("X-Transformed".to_string(), Some("fetch".to_string()));
            Box::pin(async move { headers })
        });
    runtime
        .fetch_deferred(
            &composed,
            &handle("fetch-id"),
            kiana_ai::models::ModelsDeferredFetchOptions {
                base: DeferredFetchOptions {
                    base: StreamOptions {
                        headers: Some(headers(&[("X-Fetch", Some("fetch"))])),
                        ..Default::default()
                    },
                    wait: Some(25),
                },
                transforms: kiana_ai::models::ModelsRequestTransforms {
                    transform_headers: Some(transform),
                },
                ..Default::default()
            },
        )
        .await;

    let cancel_transform: kiana_ai::models::TransformHeadersFn =
        Arc::new(|mut headers: ProviderHeaders| {
            headers.insert("X-Transformed".to_string(), Some("cancel".to_string()));
            Box::pin(async move { headers })
        });
    runtime
        .cancel_deferred(
            &composed,
            &handle("cancel-id"),
            kiana_ai::models::ModelsDeferredCancelOptions {
                base: DeferredCancelOptions {
                    timeout_ms: Some(100),
                    ..Default::default()
                },
                transforms: kiana_ai::models::ModelsRequestTransforms {
                    transform_headers: Some(cancel_transform),
                },
                ..Default::default()
            },
        )
        .await
        .expect("cancel");

    let (fetched_base_url, fetched_api_key, fetched_headers, fetched_wait) =
        fetched.lock().unwrap().clone().expect("fetch captured");
    assert_eq!(fetched_base_url, "https://overlay.test/v1");
    assert_eq!(fetched_wait, Some(25));
    assert_eq!(fetched_api_key.as_deref(), Some("key"));
    let fetched_headers = fetched_headers.expect("fetch headers");
    assert_eq!(
        header_value(&fetched_headers, "X-Fetch"),
        Some(Some("fetch"))
    );
    assert_eq!(
        header_value(&fetched_headers, "X-Transformed"),
        Some(Some("fetch"))
    );

    let (cancelled_id, cancelled_api_key, cancelled_headers, cancelled_timeout) =
        cancelled.lock().unwrap().clone().expect("cancel captured");
    assert_eq!(cancelled_id, "cancel-id");
    assert_eq!(cancelled_timeout, Some(100));
    assert_eq!(cancelled_api_key.as_deref(), Some("key"));
    let cancelled_headers = cancelled_headers.expect("cancel headers");
    assert_eq!(
        header_value(&cancelled_headers, "X-Transformed"),
        Some(Some("cancel"))
    );
    assert!(header_value(&cancelled_headers, "X-Fetch").is_none());
}

#[tokio::test]
async fn applies_models_json_overrides_above_native_providers() {
    let harness = Harness::new();
    harness.write_providers(json!({
        "extension-native": { "modelOverrides": { "native": { "contextWindow": 4242 } } }
    }));
    let runtime = harness.create().await;
    let native_model = Model {
        provider: "extension-native".to_string(),
        base_url: "https://native.test/v1".to_string(),
        ..model("native")
    };
    let auth = TestApiKeyAuth::new("Native key").with_resolve(Arc::new(|_credential| {
        Box::pin(async {
            Ok(Some(kiana_ai::auth::types::AuthResult::new(
                kiana_ai::auth::types::ModelAuth {
                    api_key: Some("key".to_string()),
                    ..Default::default()
                },
            )))
        })
    }));
    runtime
        .register_native_provider(
            TestProvider::new(
                "extension-native",
                Arc::new(auth).provider_auth(),
                vec![native_model],
            )
            .with_name("Extension Native")
            .into_provider(),
        )
        .expect("register");

    assert_eq!(
        runtime
            .get_model("extension-native", "native")
            .expect("model")
            .context_window,
        Some(4242)
    );
}

#[tokio::test]
async fn publishes_refresh_models_results_without_forcing_models_store_persistence() {
    let harness = Harness::new();
    let runtime = harness.create().await;

    let refresh_models: Arc<kiana_config::models::composer::ExtensionRefreshModelsFn> =
        Arc::new(|_context| {
            Box::pin(async {
                Ok(vec![ProviderModelConfig::Chat(
                    kiana_config::models::composer::ProviderChatModelConfig {
                        base: kiana_config::models::composer::ProviderModelConfigBase {
                            id: "live".to_string(),
                            name: "live".to_string(),
                            api: None,
                            base_url: Some("http://localhost:8080/v1".to_string()),
                            input: vec![kiana_ai::types::ModelInputModality::Text],
                            input_limits: None,
                            cost: kiana_ai::types::ModelCost::default(),
                            headers: None,
                            declared_type: None,
                        },
                        reasoning: false,
                        thinking_level_map: None,
                        prompt_cache: None,
                        context_window: 1000.0,
                        max_tokens: 100.0,
                        sampling_params: None,
                        sampling_params_by_thinking_level: None,
                        compat: None,
                    },
                )])
            })
        });
    runtime
        .register_provider(
            "extension-dynamic",
            ProviderConfigInput {
                base_url: Some("http://localhost:8080/v1".to_string()),
                api_key: Some("local".to_string()),
                api: Some("openai-completions".to_string()),
                refresh_models: Some(refresh_models),
                ..Default::default()
            },
        )
        .expect("register");

    runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        })
        .await
        .expect("refresh");

    assert!(runtime.get_model("extension-dynamic", "live").is_some());
    assert!(
        harness
            .models_store
            .read("extension-dynamic", ModelsStoreOperationOptions::default())
            .await
            .expect("read")
            .is_none()
    );
}

#[tokio::test]
async fn applies_legacy_oauth_modify_models_after_async_credential_initialization() {
    let harness = Harness::with_auth(json!({
        "extension-oauth": {
            "type": "oauth",
            "access": "access",
            "refresh": "refresh",
            "expires": now_ms() + 60_000
        }
    }));
    let runtime = harness.create().await;

    let login: Arc<ExtensionLoginFn> = Arc::new(|_callbacks| {
        Box::pin(async { Err(kiana_ai::auth::types::AuthError::new("not used")) })
    });
    let refresh_token: Arc<ExtensionRefreshTokenFn> =
        Arc::new(|credential, _signal| Box::pin(async move { Ok(credential) }));
    let get_api_key: Arc<ExtensionGetApiKeyFn> = Arc::new(|credential| credential.access.clone());
    let modify_models: Arc<kiana_config::models::composer::ExtensionModifyModelsFn> =
        Arc::new(|mut models, credential| {
            if credential.access == "access" {
                models.push(model("credential-model"));
            }
            models
        });
    runtime
        .register_provider(
            "extension-oauth",
            ProviderConfigInput {
                base_url: Some("https://example.test/v1".to_string()),
                api: Some("openai-completions".to_string()),
                models: Some(vec![ProviderModelConfig::Chat(
                    kiana_config::models::composer::ProviderChatModelConfig {
                        base: kiana_config::models::composer::ProviderModelConfigBase {
                            id: "base".to_string(),
                            name: "base".to_string(),
                            api: None,
                            base_url: None,
                            input: vec![kiana_ai::types::ModelInputModality::Text],
                            input_limits: None,
                            cost: kiana_ai::types::ModelCost::default(),
                            headers: None,
                            declared_type: None,
                        },
                        reasoning: false,
                        thinking_level_map: None,
                        prompt_cache: None,
                        context_window: 1000.0,
                        max_tokens: 100.0,
                        sampling_params: None,
                        sampling_params_by_thinking_level: None,
                        compat: None,
                    },
                )]),
                oauth: Some(ExtensionOAuthConfig {
                    name: "Extension OAuth".to_string(),
                    is_subscription: None,
                    uses_callback_server: None,
                    login,
                    refresh_token,
                    get_api_key,
                    modify_models: Some(modify_models),
                }),
                ..Default::default()
            },
        )
        .expect("register");

    runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        })
        .await
        .expect("refresh");
    assert!(runtime.get_model("extension-oauth", "base").is_some());
    assert!(
        runtime
            .get_model("extension-oauth", "credential-model")
            .is_some()
    );

    runtime
        .logout("extension-oauth", AuthOperationOptions::default())
        .await
        .expect("logout");
    assert!(
        runtime
            .get_model("extension-oauth", "credential-model")
            .is_none()
    );
}

// 保持 `OAuthCredentials` / `SimpleStreamOptions` / `TranscriptContext` / `ModelType` /
// `AssistantMessage` 类型被引用（夹具签名的一部分）。
#[allow(dead_code)]
fn _type_aliases(
    credentials: OAuthCredentials,
    simple: SimpleStreamOptions,
    transcript: TranscriptContext,
    model_type: ModelType,
    message: AssistantMessage,
) -> (
    OAuthCredentials,
    SimpleStreamOptions,
    TranscriptContext,
    ModelType,
    AssistantMessage,
) {
    (credentials, simple, transcript, model_type, message)
}
