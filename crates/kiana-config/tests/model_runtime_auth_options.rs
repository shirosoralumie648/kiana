//! `model-runtime-auth-options.test.ts` 的移植（11 个用例）。
//!
//! 另含 `sdk-client-identity.test.ts` 中 2 个 OpenCode session 头用例（改写到
//! `ModelRuntime::prepare_request`，因为 SDK 会话装配不在本单元范围内）。

#[path = "fixtures/model_runtime/mod.rs"]
mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use indexmap::IndexMap;
use kiana_ai::auth::credential_store::InMemoryCredentialStore;
use kiana_ai::auth::types::{
    ApiKeyCredential, AuthOperationOptions, AuthType, Credential, CredentialModifyFn,
    CredentialStore,
};
use kiana_ai::models::{ModelsRequestTransforms, ModelsSimpleStreamOptions, TransformHeadersFn};
use kiana_ai::types::{Model, ProviderHeaders, SimpleStreamOptions, StreamOptions};
use kiana_config::auth::storage::{AuthStorage, auth_data_from_value};
use kiana_config::models::composer::{
    ExtensionGetApiKeyFn, ExtensionLoginFn, ExtensionOAuthConfig, ExtensionRefreshTokenFn,
    ProviderConfigInput, ProviderModelConfig,
};
use kiana_config::models::registry::ModelRegistry;
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime};
use serde_json::json;
use support::*;

/// 只含凭证与 `modelsPath: null` 的 runtime（TS `ModelRuntime.create({credentials, modelsPath:null})`）。
async fn runtime_with_store(store: Arc<dyn CredentialStore>) -> Arc<ModelRuntime> {
    Arc::new(
        ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(store),
            models_path: kiana_config::models::runtime::ModelsPathOption::Disabled,
            ..Default::default()
        })
        .await
        .expect("runtime"),
    )
}

/// 扩展 provider 的 `streamSimple`：把收到的 headers 发给测试。
fn capturing_headers_provider(
    base_url: &str,
    api_key: &str,
    auth_header: bool,
    provider_headers: Option<IndexMap<String, String>>,
    models: Vec<ProviderModelConfig>,
) -> (
    ProviderConfigInput,
    tokio::sync::mpsc::UnboundedReceiver<Option<ProviderHeaders>>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let tx = Mutex::new(Some(tx));
    let stream_simple: Arc<kiana_config::models::composer::ExtensionStreamSimpleFn> = Arc::new(
        move |_model: &Model,
              _context: &kiana_ai::types::TranscriptContext,
              options: SimpleStreamOptions| {
            if let Some(tx) = tx
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
            {
                let _ = tx.send(options.base.headers.clone());
            }
            kiana_ai::event_stream::AssistantMessageEventStream::new()
        },
    );
    (
        ProviderConfigInput {
            base_url: Some(base_url.to_string()),
            api: Some("openai-completions".to_string()),
            api_key: Some(api_key.to_string()),
            auth_header: Some(auth_header),
            headers: provider_headers,
            stream_simple: Some(stream_simple),
            models: Some(models),
            ..Default::default()
        },
        rx,
    )
}

/// 等待 provider 收到请求（5 秒超时）。
async fn receive_headers(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Option<ProviderHeaders>>,
) -> Option<ProviderHeaders> {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("provider dispatch")
        .expect("headers message")
}

// ---------------------------------------------------------------------------
// 11 个 auth-options 用例
// ---------------------------------------------------------------------------

#[tokio::test]
async fn accepts_a_kiana_ai_credential_store() {
    let credentials = Arc::new(InMemoryCredentialStore::new());
    credentials
        .modify(
            "anthropic",
            Box::new(|_current: Option<Credential>| {
                Box::pin(async {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("stored-key".to_string()),
                        env: None,
                        extra: IndexMap::new(),
                    })))
                }) as kiana_ai::auth::types::CredentialModifyFuture
            }) as CredentialModifyFn,
            AuthOperationOptions::default(),
        )
        .await
        .expect("modify");
    let runtime = runtime_with_store(credentials).await;

    let auth = runtime
        .get_auth_for_provider("anthropic", Default::default())
        .await
        .expect("auth")
        .expect("configured");
    assert_eq!(auth.auth.api_key, Some("stored-key".to_string()));
}

/// 记录 read 调用的包装 store。
struct RecordingStore {
    base: Arc<InMemoryCredentialStore>,
    reads: Mutex<Vec<String>>,
    fail_reads: std::sync::atomic::AtomicBool,
}

impl RecordingStore {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            base: Arc::new(InMemoryCredentialStore::new()),
            reads: Mutex::new(Vec::new()),
            fail_reads: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn reads(&self) -> Vec<String> {
        self.reads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn clear_reads(&self) {
        self.reads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    fn set_fail_reads(&self, fail: bool) {
        self.fail_reads.store(fail, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl CredentialStore for RecordingStore {
    async fn read(
        &self,
        provider_id: &str,
        options: AuthOperationOptions,
    ) -> Result<Option<Credential>, kiana_ai::auth::types::AuthError> {
        self.reads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(provider_id.to_string());
        if self.fail_reads.load(Ordering::SeqCst) {
            return Err(kiana_ai::auth::types::AuthError::new(format!(
                "read failed for {provider_id}"
            )));
        }
        self.base.read(provider_id, options).await
    }

    async fn list(
        &self,
        options: AuthOperationOptions,
    ) -> Result<Vec<kiana_ai::auth::types::CredentialInfo>, kiana_ai::auth::types::AuthError> {
        self.base.list(options).await
    }

    async fn modify(
        &self,
        provider_id: &str,
        f: CredentialModifyFn,
        options: AuthOperationOptions,
    ) -> Result<Option<Credential>, kiana_ai::auth::types::AuthError> {
        self.base.modify(provider_id, f, options).await
    }

    async fn delete(
        &self,
        provider_id: &str,
        options: AuthOperationOptions,
    ) -> Result<(), kiana_ai::auth::types::AuthError> {
        self.base.delete(provider_id, options).await
    }
}

#[tokio::test]
async fn scopes_provider_availability_reads_and_records_refresh_failures() {
    let credentials = RecordingStore::new();
    let runtime = runtime_with_store(Arc::clone(&credentials) as Arc<dyn CredentialStore>).await;

    credentials.clear_reads();
    runtime
        .get_available(Some("anthropic"), AuthOperationOptions::default())
        .await
        .expect("available");
    let unique: std::collections::HashSet<String> = credentials.reads().into_iter().collect();
    assert_eq!(
        unique,
        std::collections::HashSet::from(["anthropic".to_string()])
    );

    credentials.set_fail_reads(true);
    let error = runtime
        .get_available(Some("anthropic"), AuthOperationOptions::default())
        .await
        .expect_err("read failure");
    assert!(
        error
            .message
            .contains("Credential store read failed for anthropic")
    );
    assert!(
        runtime
            .get_error()
            .expect("error")
            .contains("Availability refresh: Credential store read failed for anthropic")
    );

    credentials.set_fail_reads(false);
    runtime
        .get_available(None, AuthOperationOptions::default())
        .await
        .expect("available");
    assert!(runtime.get_error().is_none());
}

/// 回归：TS `getAvailable(providerId?)` 的 `if (providerId)` 把空串当作未给出——走全量
/// 可用性刷新并更新快照（修复前走 provider 作用域分支：模型照常返回，但快照保持 stale）。
#[tokio::test]
async fn empty_provider_id_takes_the_full_availability_refresh_path() {
    let credentials = Arc::new(InMemoryCredentialStore::new());
    credentials
        .modify(
            "anthropic",
            Box::new(|_current: Option<Credential>| {
                Box::pin(async {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("stored-key".to_string()),
                        env: None,
                        extra: IndexMap::new(),
                    })))
                }) as kiana_ai::auth::types::CredentialModifyFuture
            }) as CredentialModifyFn,
            AuthOperationOptions::default(),
        )
        .await
        .expect("modify");
    // `refreshOnCreate: false`：快照从空开始，全量刷新是否落地可直接观察。
    let runtime = Arc::new(
        ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(credentials),
            models_path: kiana_config::models::runtime::ModelsPathOption::Disabled,
            refresh_on_create: Some(false),
            ..Default::default()
        })
        .await
        .expect("runtime"),
    );
    assert!(runtime.get_available_snapshot().is_empty());

    let via_empty = runtime
        .get_available(Some(""), AuthOperationOptions::default())
        .await
        .expect("available");
    let snapshot = runtime.get_available_snapshot();
    assert_eq!(via_empty, snapshot);
    assert!(
        snapshot.iter().any(|model| model.provider == "anthropic"),
        "full refresh must publish the available catalog"
    );
}

#[tokio::test]
async fn projects_provider_owned_methods_names_and_status() {
    let runtime = runtime_with_store(Arc::new(InMemoryCredentialStore::new())).await;

    let bedrock = runtime.get_provider("amazon-bedrock").expect("bedrock");
    assert_eq!(bedrock.name(), "Amazon Bedrock");
    assert_eq!(
        bedrock.auth().api_key.as_ref().expect("api key").name(),
        "AWS credentials or bearer token"
    );

    let vertex = runtime.get_provider("google-vertex").expect("vertex");
    assert_eq!(vertex.name(), "Google Vertex AI");
    assert_eq!(
        vertex.auth().api_key.as_ref().expect("api key").name(),
        "Google Cloud credentials"
    );

    let anthropic = runtime.get_provider("anthropic").expect("anthropic");
    assert_eq!(anthropic.name(), "Anthropic");
    assert!(anthropic.auth().oauth.is_some());

    let gateway = runtime
        .get_provider("cloudflare-ai-gateway")
        .expect("gateway");
    assert_eq!(gateway.name(), "Cloudflare AI Gateway");
    assert!(gateway.auth().api_key.is_some());

    let workers = runtime
        .get_provider("cloudflare-workers-ai")
        .expect("workers");
    assert_eq!(workers.name(), "Cloudflare Workers AI");
    assert!(workers.auth().api_key.is_some());

    let codex = runtime.get_provider("openai-codex").expect("codex");
    assert!(codex.auth().api_key.is_none());
}

#[tokio::test]
async fn attaches_the_providers_active_auth_status_to_every_method_option() {
    let runtime = runtime_with_store(Arc::new(AuthStorage::in_memory(auth_data_from_value(
        json!({
            "anthropic": {
                "type": "oauth",
                "access": "access",
                "refresh": "refresh",
                "expires": now_ms() + 60_000
            }
        }),
    ))))
    .await;

    let anthropic = runtime.get_provider("anthropic").expect("anthropic");
    let methods = [
        anthropic.auth().oauth.is_some(),
        anthropic.auth().api_key.is_some(),
    ];
    assert_eq!(methods, [true, true]);

    let check = runtime
        .check_auth("anthropic", AuthOperationOptions::default())
        .await
        .expect("check")
        .expect("configured");
    assert_eq!(check.r#type, AuthType::OAuth);
}

#[tokio::test]
async fn distinguishes_subscription_oauth_from_generic_oauth_sign_in() {
    let runtime = runtime_with_store(Arc::new(AuthStorage::in_memory(auth_data_from_value(
        json!({
            "anthropic": {
                "type": "oauth",
                "access": "anthropic-access",
                "refresh": "anthropic-refresh",
                "expires": now_ms() + 60 * 60_000
            },
            "openrouter": {
                "type": "oauth",
                "access": "openrouter-key",
                "refresh": "",
                "expires": 9007199254740991i64
            },
            "radius": {
                "type": "oauth",
                "access": "radius-access",
                "refresh": "radius-refresh",
                "expires": now_ms() + 60 * 60_000
            }
        }),
    ))))
    .await;

    assert!(runtime.is_using_oauth("anthropic"));
    assert!(runtime.is_using_subscription("anthropic"));
    assert!(runtime.is_using_oauth("openrouter"));
    assert!(!runtime.is_using_subscription("openrouter"));
    assert!(runtime.is_using_oauth("radius"));
    assert!(!runtime.is_using_subscription("radius"));
}

#[tokio::test]
async fn constructs_an_api_key_method_for_an_extension_api_key_provider() {
    let runtime = runtime_with_store(Arc::new(AuthStorage::in_memory(auth_data_from_value(
        json!({}),
    ))))
    .await;
    runtime
        .register_provider(
            "extension-api-key",
            ProviderConfigInput {
                name: Some("Extension API Key".to_string()),
                base_url: Some("https://example.test/v1".to_string()),
                api_key: Some("$EXTENSION_TEST_API_KEY".to_string()),
                api: Some("openai-completions".to_string()),
                models: Some(vec![ProviderModelConfig::Chat(
                    kiana_config::models::composer::ProviderChatModelConfig {
                        base: kiana_config::models::composer::ProviderModelConfigBase {
                            id: "extension-model".to_string(),
                            name: "extension-model".to_string(),
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
                        context_window: 10000.0,
                        max_tokens: 1000.0,
                        sampling_params: None,
                        sampling_params_by_thinking_level: None,
                        compat: None,
                    },
                )]),
                ..Default::default()
            },
        )
        .expect("register");

    let provider = runtime
        .get_provider("extension-api-key")
        .expect("extension provider");
    assert_eq!(provider.name(), "Extension API Key");
    assert!(provider.auth().oauth.is_none());
    let api_key = provider.auth().api_key.as_ref().expect("api key method");
    assert_eq!(api_key.name(), "API key");
    assert!(api_key.has_login());
}

#[tokio::test]
async fn resolves_configured_auth_from_request_scoped_environment_overrides() {
    let runtime = runtime_with_store(Arc::new(AuthStorage::in_memory(auth_data_from_value(
        json!({}),
    ))))
    .await;
    runtime
        .register_provider(
            "request-env-provider",
            ProviderConfigInput {
                base_url: Some("https://example.test/v1".to_string()),
                api_key: Some("$REQUEST_SCOPED_API_KEY".to_string()),
                headers: Some(IndexMap::from([(
                    "x-request-value".to_string(),
                    "$REQUEST_SCOPED_HEADER".to_string(),
                )])),
                api: Some("openai-completions".to_string()),
                models: Some(vec![chat_model_config(
                    "request-env-model",
                    "request-env-model",
                )]),
                ..Default::default()
            },
        )
        .expect("register");

    let auth = runtime
        .get_auth_for_provider(
            "request-env-provider",
            kiana_ai::auth::resolve::AuthResolutionOverrides {
                env: Some(IndexMap::from([
                    (
                        "REQUEST_SCOPED_API_KEY".to_string(),
                        "request-key".to_string(),
                    ),
                    (
                        "REQUEST_SCOPED_HEADER".to_string(),
                        "request-header".to_string(),
                    ),
                ])),
                ..Default::default()
            },
        )
        .await
        .expect("auth")
        .expect("configured");

    assert_eq!(auth.auth.api_key, Some("request-key".to_string()));
    let headers = auth.auth.headers.expect("headers");
    assert_eq!(
        header_value(&headers, "x-request-value"),
        Some(Some("request-header"))
    );
}

#[tokio::test]
async fn lets_an_explicit_authorization_header_override_auth_header_case_insensitively() {
    let runtime = runtime_with_store(Arc::new(AuthStorage::in_memory(auth_data_from_value(
        json!({}),
    ))))
    .await;
    let (config, mut captured) = capturing_headers_provider(
        "https://example.test/v1",
        "generated-key",
        true,
        None,
        vec![chat_model_config("auth-header-model", "auth-header-model")],
    );
    runtime
        .register_provider("auth-header-provider", config)
        .expect("register");
    let model = runtime
        .get_model("auth-header-provider", "auth-header-model")
        .expect("model");

    let _stream = runtime.stream_simple(
        &model,
        &kiana_ai::types::Context::default(),
        ModelsSimpleStreamOptions {
            base: SimpleStreamOptions {
                base: StreamOptions {
                    headers: Some(headers(&[("authorization", Some("Explicit token"))])),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
    );

    let captured = receive_headers(&mut captured).await.expect("headers");
    assert_eq!(
        header_value(&captured, "authorization"),
        Some(Some("Explicit token"))
    );
    assert_eq!(
        header_value(&captured, "Authorization"),
        Some(Some("Explicit token"))
    );
    assert_eq!(captured.len(), 1);
}

#[tokio::test]
async fn transforms_fully_assembled_headers_once_without_forwarding_the_transform() {
    let runtime = runtime_with_store(Arc::new(AuthStorage::in_memory(auth_data_from_value(
        json!({}),
    ))))
    .await;
    let mut model_config = chat_model_config("header-model", "header-model");
    if let ProviderModelConfig::Chat(chat) = &mut model_config {
        chat.base.headers = Some(IndexMap::from([(
            "x-model".to_string(),
            "model".to_string(),
        )]));
    }
    let (config, mut captured) = capturing_headers_provider(
        "https://example.test/v1",
        "generated-key",
        true,
        Some(IndexMap::from([(
            "x-provider".to_string(),
            "provider".to_string(),
        )])),
        vec![model_config],
    );
    runtime
        .register_provider("header-provider", config)
        .expect("register");
    let model = runtime
        .get_model("header-provider", "header-model")
        .expect("model");

    let transforms = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&transforms);
    let transform: TransformHeadersFn = Arc::new(move |headers: ProviderHeaders| {
        counter.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            header_value(&headers, "Authorization"),
            Some(Some("Bearer generated-key"))
        );
        assert_eq!(header_value(&headers, "x-provider"), Some(Some("provider")));
        assert_eq!(header_value(&headers, "x-model"), Some(Some("model")));
        assert_eq!(header_value(&headers, "x-explicit"), Some(Some("explicit")));
        let mut next = headers;
        next.insert("x-transformed".to_string(), Some("yes".to_string()));
        Box::pin(async move { next })
    });
    let _stream = runtime.stream_simple(
        &model,
        &kiana_ai::types::Context::default(),
        ModelsSimpleStreamOptions {
            base: SimpleStreamOptions {
                base: StreamOptions {
                    headers: Some(headers(&[("x-explicit", Some("explicit"))])),
                    ..Default::default()
                },
                ..Default::default()
            },
            transforms: ModelsRequestTransforms {
                transform_headers: Some(transform),
            },
            ..Default::default()
        },
    );

    let captured = receive_headers(&mut captured).await.expect("headers");
    assert_eq!(transforms.load(Ordering::SeqCst), 1);
    assert_eq!(
        header_value(&captured, "Authorization"),
        Some(Some("Bearer generated-key"))
    );
    assert_eq!(
        header_value(&captured, "x-provider"),
        Some(Some("provider"))
    );
    assert_eq!(header_value(&captured, "x-model"), Some(Some("model")));
    assert_eq!(
        header_value(&captured, "x-explicit"),
        Some(Some("explicit"))
    );
    assert_eq!(header_value(&captured, "x-transformed"), Some(Some("yes")));
}

#[tokio::test]
async fn does_not_cancel_an_extension_oauth_refresh_with_the_request() {
    let credentials = Arc::new(AuthStorage::in_memory(auth_data_from_value(json!({
        "extension-oauth": {
            "type": "oauth",
            "access": "expired",
            "refresh": "refresh",
            "expires": 0
        }
    }))));
    let runtime = runtime_with_store(Arc::clone(&credentials) as Arc<dyn CredentialStore>).await;

    let refresh_signal: Arc<Mutex<Option<kiana_ai::utils::abort::AbortScope>>> =
        Arc::new(Mutex::new(None));
    let signal_slot = Arc::clone(&refresh_signal);
    let refresh_token: Arc<ExtensionRefreshTokenFn> = Arc::new(move |mut credential, signal| {
        *signal_slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(signal);
        credential.expires = (now_ms() + 60_000) as f64;
        Box::pin(async move { Ok(credential) })
    });
    let login: Arc<ExtensionLoginFn> = Arc::new(|_callbacks| {
        Box::pin(async {
            Ok(kiana_ai::auth::types::OAuthCredentials {
                access: "access".into(),
                refresh: "refresh".into(),
                expires: 0.0,
                extra: IndexMap::new(),
            })
        })
    });
    let get_api_key: Arc<ExtensionGetApiKeyFn> = Arc::new(|credential| credential.access.clone());
    runtime
        .register_provider(
            "extension-oauth",
            ProviderConfigInput {
                name: Some("Extension OAuth".to_string()),
                base_url: Some("https://example.test/v1".to_string()),
                api: Some("openai-completions".to_string()),
                oauth: Some(ExtensionOAuthConfig {
                    name: "Extension subscription".to_string(),
                    is_subscription: None,
                    uses_callback_server: None,
                    login,
                    refresh_token,
                    get_api_key,
                    modify_models: None,
                }),
                models: Some(vec![chat_model_config(
                    "extension-model",
                    "extension-model",
                )]),
                ..Default::default()
            },
        )
        .expect("register");

    let controller = kiana_ai::utils::abort::AbortScope::new();
    runtime
        .get_auth_for_provider(
            "extension-oauth",
            kiana_ai::auth::resolve::AuthResolutionOverrides {
                signal: Some(controller.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("auth");

    let signal = refresh_signal
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .expect("refresh was started");
    assert!(!signal.is_aborted());

    controller.abort();
    // A started refresh may already have rotated the refresh token and must be persisted.
    assert!(!signal.is_aborted());
}

#[tokio::test]
async fn does_not_fabricate_an_api_key_method_for_an_extension_oauth_only_provider() {
    let runtime = runtime_with_store(Arc::new(AuthStorage::in_memory(auth_data_from_value(
        json!({}),
    ))))
    .await;
    runtime
        .register_provider(
            "extension-oauth",
            ProviderConfigInput {
                name: Some("Extension OAuth".to_string()),
                base_url: Some("https://example.test/v1".to_string()),
                api: Some("openai-completions".to_string()),
                oauth: Some(ExtensionOAuthConfig {
                    name: "Extension subscription".to_string(),
                    is_subscription: Some(true),
                    uses_callback_server: None,
                    login: Arc::new(|_callbacks| {
                        Box::pin(async {
                            Ok(kiana_ai::auth::types::OAuthCredentials {
                                access: "access".into(),
                                refresh: "refresh".into(),
                                expires: 0.0,
                                extra: IndexMap::new(),
                            })
                        })
                    }),
                    refresh_token: Arc::new(|credential, _signal| {
                        Box::pin(async move { Ok(credential) })
                    }),
                    get_api_key: Arc::new(|credential| credential.access.clone()),
                    modify_models: None,
                }),
                models: Some(vec![chat_model_config(
                    "extension-model",
                    "extension-model",
                )]),
                ..Default::default()
            },
        )
        .expect("register");

    let provider = runtime.get_provider("extension-oauth").expect("provider");
    assert_eq!(provider.name(), "Extension OAuth");
    assert!(provider.auth().api_key.is_none());
    let oauth = provider.auth().oauth.as_ref().expect("oauth method");
    assert_eq!(oauth.name(), "Extension subscription");
    assert!(oauth.is_subscription());
}

// ---------------------------------------------------------------------------
// 2 个 OpenCode session 头用例（sdk-client-identity.test.ts）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn adds_open_code_session_headers_for_custom_open_code_providers() {
    let runtime = runtime_with_store(Arc::new(AuthStorage::in_memory(auth_data_from_value(
        json!({}),
    ))))
    .await;
    runtime
        .register_provider(
            "custom-opencode",
            ProviderConfigInput {
                base_url: Some("https://opencode.ai/zen/v1".to_string()),
                api_key: Some("test-key".to_string()),
                api: Some("openai-completions".to_string()),
                models: Some(vec![chat_model_config(
                    "custom-opencode-test-model",
                    "custom-opencode-test-model",
                )]),
                ..Default::default()
            },
        )
        .expect("register");
    let model = runtime
        .get_model("custom-opencode", "custom-opencode-test-model")
        .expect("model");

    let prepared = runtime
        .prepare_request(
            &model,
            kiana_ai::models::ModelsStreamOptions {
                base: StreamOptions {
                    session_id: Some("opencode-session".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .expect("prepare");

    let headers = prepared.options.headers.expect("headers");
    assert_eq!(
        header_value(&headers, "x-opencode-session"),
        Some(Some("opencode-session"))
    );
}

#[tokio::test]
async fn lets_configured_open_code_headers_override_the_session_header() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "opencode": {
            "headers": { "x-opencode-session": "configured-session" }
        }}
    }));
    let runtime = harness.create().await;
    let model = runtime
        .get_models(Some("opencode"))
        .into_iter()
        .next()
        .expect("opencode model");
    // SDK 会话在请求前已解析凭证；这里直接给内置 provider 一个运行时 key。
    runtime
        .set_runtime_api_key("opencode", "sk-opencode", AuthOperationOptions::default())
        .await
        .expect("runtime key");

    let prepared = runtime
        .prepare_request(
            &model,
            kiana_ai::models::ModelsStreamOptions {
                base: StreamOptions {
                    session_id: Some("opencode-session".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .expect("prepare");

    let headers = prepared.options.headers.expect("headers");
    assert_eq!(
        header_value(&headers, "x-opencode-session"),
        Some(Some("configured-session"))
    );
}

// 保持 `ModelRegistry` 的公开类型被引用（与 authOptions helper 的 provider 投影对应）。
fn _registry_alias(registry: ModelRegistry) -> ModelRegistry {
    registry
}
