//! `provider-composer.ts` 的 spec 派生测试（TS 侧没有专门的 `provider-composer.test.ts`）。
//!
//! 覆盖四层合成（models.json → extension → OAuth modifyModels → modelOverrides）、
//! 认证组合（apiKey/OAuth/继承/请求头）、逐字错误文案与流分派。

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthCheck, AuthContext, AuthError, AuthEvent,
    AuthInteraction, AuthPromptRequest, AuthResult, AuthType, ModelAuth, OAuthAuth,
    OAuthCredential, ProviderAuth,
};
use kiana_ai::compat::extension_oauth_types::{
    OAuthAuthInfo, OAuthCredentials, OAuthDeviceCodeInfo, OAuthPrompt, OAuthSelectOption,
    OAuthSelectPrompt,
};
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::models::{
    CreateProviderOptions, ModelsPublication, Provider, ProviderApiImpl, ProviderStreams,
    RefreshModelsContext, create_provider,
};
use kiana_ai::types::{
    AnyModel, ClassifierContext, ClassifierStopReason, ImagesContext, ImagesStopReason, Model,
    ModelCost, ModelInputModality, ModelType, ProviderEnv, SimpleStreamOptions, StreamOptions,
    TranscriptContext,
};
use kiana_ai::utils::abort::AbortScope;
use kiana_config::models::composer::{
    AuthStatusSource, ExtensionLoginFn, ExtensionLoginFuture, ExtensionOAuthCallbacks,
    ExtensionOAuthConfig, ExtensionRefreshTokenFn, ProviderChatModelConfig, ProviderConfigInput,
    ProviderImageModelConfig, ProviderModelConfig, ProviderModelConfigBase, clear_api_key_cache,
    compose_model_provider, configured_request_auth_status, resolve_compatibility_request_config,
    resolve_configured_model_headers, validate_extension_provider,
};
use kiana_config::models::config_file::{JsonHeaders, ModelConfig};
use serde_json::{Value, json};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// 测试替身
// ---------------------------------------------------------------------------

struct TestStreams;

impl ProviderStreams for TestStreams {
    fn stream(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: StreamOptions,
    ) -> AssistantMessageEventStream {
        panic!("stream implementation is not used in these tests")
    }

    fn stream_simple(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        panic!("stream implementation is not used in these tests")
    }
}

#[derive(Default)]
struct TestAuthContext {
    env: ProviderEnv,
}

#[async_trait]
impl AuthContext for TestAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        self.env.get(name).cloned()
    }

    async fn file_exists(&self, _path: &str) -> bool {
        false
    }
}

struct RecordingInteraction {
    signal: AbortScope,
    answers: Mutex<VecDeque<String>>,
    prompts: Mutex<Vec<AuthPromptRequest>>,
    events: Mutex<Vec<AuthEvent>>,
}

impl RecordingInteraction {
    fn new(answers: Vec<&str>) -> Self {
        Self {
            signal: AbortScope::new(),
            answers: Mutex::new(answers.into_iter().map(str::to_string).collect()),
            prompts: Mutex::new(Vec::new()),
            events: Mutex::new(Vec::new()),
        }
    }

    fn prompt_types(&self) -> Vec<String> {
        self.prompts
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.prompt.type_name().to_string())
            .collect()
    }

    fn messages(&self) -> Vec<String> {
        self.prompts
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.prompt.message().to_string())
            .collect()
    }
}

#[async_trait]
impl AuthInteraction for RecordingInteraction {
    fn signal(&self) -> &AbortScope {
        &self.signal
    }

    async fn prompt(&self, request: AuthPromptRequest) -> Result<String, AuthError> {
        self.prompts.lock().unwrap().push(request);
        Ok(self
            .answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| "answer".to_string()))
    }

    fn notify(&self, event: &AuthEvent) {
        self.events.lock().unwrap().push(event.clone());
    }
}

/// 静态 auth：`resolve` 返回固定结果。
struct StaticAuth {
    name: String,
    result: Option<AuthResult>,
}

#[async_trait]
impl ApiKeyAuth for StaticAuth {
    fn name(&self) -> &str {
        &self.name
    }

    async fn resolve(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        Ok(self.result.clone())
    }
}

/// 带 login/check/resolve 的继承 auth。
struct InheritedAuth;

#[async_trait]
impl ApiKeyAuth for InheritedAuth {
    fn name(&self) -> &str {
        "Inherited key"
    }

    fn has_login(&self) -> bool {
        true
    }

    async fn login(
        &self,
        _interaction: &dyn AuthInteraction,
    ) -> Result<ApiKeyCredential, AuthError> {
        Ok(ApiKeyCredential {
            key: Some("inherited-login".to_string()),
            env: None,
            extra: IndexMap::new(),
        })
    }

    fn has_check(&self) -> bool {
        true
    }

    async fn check(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthCheck>, AuthError> {
        Ok(Some(AuthCheck {
            source: Some("inherited check".to_string()),
            r#type: AuthType::ApiKey,
        }))
    }

    async fn resolve(&self, input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some(
                    input
                        .credential
                        .and_then(|credential| credential.key.clone())
                        .unwrap_or_else(|| "inherited-key".to_string()),
                ),
                ..ModelAuth::default()
            },
            env: None,
            source: Some("inherited source".to_string()),
        }))
    }
}

/// 固定 OAuth auth（用于纯 OAuth provider 与继承）。
struct StaticOAuth {
    name: String,
}

#[async_trait]
impl OAuthAuth for StaticOAuth {
    fn name(&self) -> &str {
        &self.name
    }

    async fn login(
        &self,
        _interaction: &dyn AuthInteraction,
        _options: &kiana_ai::auth::types::LoginOptions,
    ) -> Result<OAuthCredential, AuthError> {
        Ok(OAuthCredential::new("access", "refresh", 1.0))
    }

    async fn refresh(
        &self,
        credential: &OAuthCredential,
        _signal: &AbortScope,
    ) -> Result<OAuthCredential, AuthError> {
        Ok(credential.clone())
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        Ok(ModelAuth {
            api_key: Some(format!("oauth-{}", credential.access)),
            ..ModelAuth::default()
        })
    }
}

// ---------------------------------------------------------------------------
// 构造辅助
// ---------------------------------------------------------------------------

fn chat(id: &str) -> Model {
    Model {
        id: id.to_string(),
        name: id.to_string(),
        api: "openai-completions".to_string(),
        provider: "demo".to_string(),
        base_url: "https://base.test/v1".to_string(),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        ..Model::default()
    }
}

fn image(id: &str) -> Model {
    Model {
        model_type: Some(ModelType::Image),
        api: "openrouter-images".to_string(),
        output: Some(vec!["image".to_string()]),
        ..chat(id)
    }
}

fn classifier(id: &str) -> Model {
    Model {
        model_type: Some(ModelType::Classifier),
        ..chat(id)
    }
}

fn base_provider(models: Vec<AnyModel>, auth: ProviderAuth) -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "demo".to_string(),
        name: Some("Demo Base".to_string()),
        auth,
        models,
        api: Some(ProviderApiImpl::Single(Arc::new(TestStreams))),
        ..CreateProviderOptions::default()
    })
    .expect("valid provider")
}

fn empty_auth() -> ProviderAuth {
    ProviderAuth {
        api_key: Some(Arc::new(StaticAuth {
            name: "Test".to_string(),
            result: Some(AuthResult::default()),
        })),
        oauth: None,
    }
}

fn no_auth() -> ProviderAuth {
    ProviderAuth::default()
}

async fn load_config(value: Value) -> (TempDir, ModelConfig) {
    let directory = TempDir::new().expect("tempdir");
    let path = directory.path().join("models.json");
    std::fs::write(&path, serde_json::to_string(&value).unwrap()).expect("write");
    let config = ModelConfig::load(Some(&path.to_string_lossy())).await;
    assert_eq!(config.get_error(), None);
    (directory, config)
}

fn provider_input() -> ProviderConfigInput {
    ProviderConfigInput::default()
}

fn model_base(id: &str) -> ProviderModelConfigBase {
    ProviderModelConfigBase {
        id: id.to_string(),
        name: id.to_string(),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        ..ProviderModelConfigBase::default()
    }
}

fn extension_chat(id: &str) -> ProviderModelConfig {
    ProviderModelConfig::Chat(ProviderChatModelConfig {
        base: model_base(id),
        reasoning: false,
        context_window: 1000.0,
        max_tokens: 100.0,
        ..ProviderChatModelConfig::default()
    })
}

fn extension_image(id: &str) -> ProviderModelConfig {
    let mut base = model_base(id);
    base.api = Some("openrouter-images".to_string());
    ProviderModelConfig::Image(ProviderImageModelConfig {
        base,
        output: vec![ModelInputModality::Image],
    })
}

fn extension_classifier(id: &str) -> ProviderModelConfig {
    let mut base = model_base(id);
    base.api = Some("typesafe-system-one".to_string());
    ProviderModelConfig::Classifier(
        kiana_config::models::composer::ProviderClassifierModelConfig {
            base,
            context_window: 64_000.0,
            compat: None,
        },
    )
}

// ---------------------------------------------------------------------------
// models.json 层

#[tokio::test]
async fn remote_sparse_json_survives_provider_and_model_overrides() {
    let (_dir, config) = load_config(json!({
        "providers": {"demo": {
            "baseUrl": "https://override.test/v1",
            "modelOverrides": {"sparse": {"name": "renamed"}},
        }}
    }))
    .await;
    let base = base_provider(
        vec![AnyModel::Remote {
            model: Model {
                id: "sparse".into(),
                provider: "demo".into(),
                ..Model::default()
            },
            json: json!({
                "id": "sparse", "provider": "demo", "futureCatalogField": {"kept": true},
            }),
        }],
        empty_auth(),
    );
    let provider = compose_model_provider("demo", Some(base), &config, None).unwrap();
    let value = serde_json::to_value(&provider.get_all_models()[0]).unwrap();
    assert_eq!(value["baseUrl"], "https://override.test/v1");
    assert_eq!(value["name"], "renamed");
    assert_eq!(value["futureCatalogField"], json!({"kept": true}));
    assert!(value.get("api").is_none(), "{value}");
    assert!(value.get("input").is_none(), "{value}");
    assert!(value.get("cost").is_none(), "{value}");
}
// ---------------------------------------------------------------------------

#[tokio::test]
async fn builds_custom_models_with_defaults() {
    let (_dir, config) = load_config(json!({
        "providers": {
            "demo": {
                "baseUrl": "https://custom.test/v1",
                "api": "openai-completions",
                "apiKey": "literal-key",
                "models": [{"id": "m"}],
            }
        }
    }))
    .await;
    let provider = compose_model_provider("demo", None, &config, None).expect("composed");
    assert_eq!(provider.id(), "demo");
    assert_eq!(provider.name(), "demo");
    assert_eq!(provider.base_url(), Some("https://custom.test/v1"));
    let models = provider.get_models();
    assert_eq!(models.len(), 1);
    let model = &models[0];
    assert_eq!(model.id, "m");
    assert_eq!(model.name, "m");
    assert_eq!(model.api, "openai-completions");
    assert_eq!(model.provider, "demo");
    assert_eq!(model.base_url, "https://custom.test/v1");
    assert_eq!(model.reasoning, Some(false));
    assert_eq!(model.input, vec![ModelInputModality::Text]);
    assert_eq!(model.cost, ModelCost::default());
    assert_eq!(model.context_window, Some(128_000));
    assert_eq!(model.max_tokens, Some(16_384));
    assert!(model.headers.is_none());
    assert!(provider.auth().api_key.is_some());
    assert!(provider.auth().oauth.is_none());
}

#[tokio::test]
async fn models_json_overrides_builtin_models() {
    let (_dir, config) = load_config(json!({
        "providers": {
            "demo": {
                "baseUrl": "https://custom.test/v1",
                "apiKey": "k",
                "compat": {"supportsStore": true, "openRouterRouting": {"order": ["b"]}},
            }
        }
    }))
    .await;
    let mut base_model = chat("static");
    base_model.compat = Some(json!({
        "supportsStore": false,
        "openRouterRouting": {"only": ["a"], "extra": 1}
    }));
    let base = base_provider(
        vec![
            AnyModel::Chat(base_model),
            AnyModel::Image(image("img")),
            AnyModel::Classifier(classifier("cls")),
        ],
        empty_auth(),
    );
    let provider = compose_model_provider("demo", Some(base), &config, None).expect("composed");
    let models = provider.get_all_models();
    let ids: Vec<&str> = models
        .iter()
        .map(|model| model.model().id.as_str())
        .collect();
    assert_eq!(ids, ["static", "img", "cls"]);
    let static_model = models[0].model();
    assert_eq!(static_model.name, "static");
    // chat：baseUrl 覆盖 + compat 一层深合并。
    assert_eq!(static_model.base_url, "https://custom.test/v1");
    assert_eq!(
        static_model.compat,
        Some(json!({
            "supportsStore": true,
            "openRouterRouting": {"only": ["a"], "extra": 1, "order": ["b"]}
        }))
    );
    // image：只改 baseUrl；classifier 无 config.compat 时保持原样。
    assert_eq!(models[1].model().base_url, "https://custom.test/v1");
    assert_eq!(models[2].model().base_url, "https://custom.test/v1");
    // classifier：provider compat 浅合并（base 没有 compat）。
    assert_eq!(
        models[2].model().compat,
        Some(json!({"supportsStore": true, "openRouterRouting": {"order": ["b"]}}))
    );
}

#[tokio::test]
async fn classifier_compat_is_a_shallow_merge() {
    let (_dir, config) = load_config(json!({
        "providers": {"demo": {"apiKey": "k", "compat": {"supportsStore": true}}}
    }))
    .await;
    let mut cls = classifier("cls");
    cls.compat = Some(json!({"nested": {"a": 1}, "supportsStore": false}));
    let base = base_provider(vec![AnyModel::Classifier(cls)], empty_auth());
    let provider = compose_model_provider("demo", Some(base), &config, None).expect("composed");
    let models = provider.get_all_models();
    assert_eq!(
        models[0].model().compat,
        Some(json!({"nested": {"a": 1}, "supportsStore": true}))
    );
}

#[tokio::test]
async fn radius_oauth_keeps_the_base_url() {
    let (_dir, config) = load_config(json!({
        "providers": {
            "demo": {"baseUrl": "https://radius.test/v1", "apiKey": "k", "oauth": "radius"}
        }
    }))
    .await;
    let base = base_provider(vec![AnyModel::Chat(chat("static"))], empty_auth());
    let provider = compose_model_provider("demo", Some(base), &config, None).expect("composed");
    assert_eq!(
        provider.get_models()[0].base_url,
        "https://base.test/v1",
        "radius OAuth must not override the built-in baseUrl"
    );
    assert!(
        provider.auth().oauth.is_none(),
        "radius OAuth comes from the runtime replacement"
    );
}

#[tokio::test]
async fn custom_model_replaces_a_builtin_chat_model_in_place() {
    let (_dir, config) = load_config(json!({
        "providers": {
            "demo": {"baseUrl": "https://custom.test/v1", "apiKey": "k",
                      "models": [{"id": "static", "name": "Replaced"}]}
        }
    }))
    .await;
    let base = base_provider(
        vec![
            AnyModel::Chat(chat("static")),
            AnyModel::Chat(chat("other")),
        ],
        empty_auth(),
    );
    let provider = compose_model_provider("demo", Some(base), &config, None).expect("composed");
    let models = provider.get_models();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "static");
    assert_eq!(models[0].name, "Replaced");
    assert_eq!(models[1].id, "other");
}

// ---------------------------------------------------------------------------
// 逐字错误
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reports_verbatim_configuration_errors() {
    let cases = [
        (
            json!({"providers": {"demo": {"baseUrl": "http://x", "apiKey": "k", "models": [{"id": "m"}]}}}),
            "Provider demo, model m: no \"api\" specified. Set at provider or model level.",
        ),
        (
            json!({"providers": {"demo": {"api": "openai-completions", "apiKey": "k", "models": [{"id": "m"}]}}}),
            "Provider demo: \"baseUrl\" is required when defining custom models.",
        ),
        (
            json!({"providers": {"demo": {"apiKey": "k", "oauth": "radius"}}}),
            "Provider demo: \"baseUrl\" is required when \"oauth\" is set.",
        ),
        (
            json!({"providers": {"demo": {"name": "only a name"}}}),
            "Provider demo: must specify \"baseUrl\", \"headers\", \"compat\", \"modelOverrides\", or \"models\".",
        ),
    ];
    for (value, expected) in cases {
        let (_dir, config) = load_config(value).await;
        let error = compose_model_provider("demo", None, &config, None)
            .err()
            .expect("must fail");
        assert_eq!(error.message, expected);
    }
}

#[tokio::test]
async fn api_key_auth_is_fabricated_even_without_any_key() {
    // TS 的 composeApiKeyAuth 只在「纯 OAuth provider」时返回 undefined；其它情况都会
    // 造一个 "API key" 登录方式（`Provider ... no authentication method configured.` 是防御分支）。
    let (_dir, config) = load_config(json!({
        "providers": {"demo": {"baseUrl": "http://x"}}
    }))
    .await;
    let base = base_provider(vec![AnyModel::Chat(chat("static"))], no_auth());
    let provider = compose_model_provider("demo", Some(base), &config, None).expect("composed");
    let auth = provider.auth().api_key.clone().expect("fabricated auth");
    assert_eq!(auth.name(), "API key");
    assert!(provider.auth().oauth.is_none());
}

#[tokio::test]
async fn extension_model_errors_are_verbatim() {
    let (_dir, config) = load_config(json!({"providers": {}})).await;
    let mut extension = provider_input();
    extension.api_key = Some("k".to_string());
    extension.models = Some(vec![extension_chat("m")]);
    let error = compose_model_provider("demo", None, &config, Some(&extension))
        .err()
        .expect("chat model without api");
    assert_eq!(
        error.message,
        "Provider demo, model m: no \"api\" specified. Set it at model level or provider level."
    );

    let mut extension = provider_input();
    extension.api_key = Some("k".to_string());
    extension.base_url = Some("http://x".to_string());
    extension.models = Some(vec![ProviderModelConfig::Image(ProviderImageModelConfig {
        base: model_base("img"),
        output: vec![ModelInputModality::Image],
    })]);
    let error = compose_model_provider("demo", None, &config, Some(&extension))
        .err()
        .expect("image model without api");
    assert_eq!(
        error.message,
        "Provider demo, model img: no \"api\" specified. Set it at model level."
    );

    let mut extension = provider_input();
    extension.api_key = Some("k".to_string());
    extension.api = Some("openai-completions".to_string());
    extension.models = Some(vec![extension_chat("m")]);
    let error = compose_model_provider("demo", None, &config, Some(&extension))
        .err()
        .expect("missing baseUrl");
    assert_eq!(
        error.message,
        "Provider demo: \"baseUrl\" is required when defining custom models."
    );
}

#[tokio::test]
async fn stream_simple_requires_api() {
    let extension = ProviderConfigInput {
        stream_simple: Some(Arc::new(|_, _, _| panic!("not used"))),
        ..ProviderConfigInput::default()
    };
    let error = validate_extension_provider("demo", None, None, &extension).expect_err("must fail");
    assert_eq!(
        error.message,
        "Provider demo: \"api\" is required when registering streamSimple."
    );
}

// ---------------------------------------------------------------------------
// extension 层
// ---------------------------------------------------------------------------

#[tokio::test]
async fn extension_model_list_is_authoritative() {
    let (_dir, config) = load_config(json!({"providers": {}})).await;
    let mut extension = provider_input();
    extension.api_key = Some("k".to_string());
    extension.api = Some("openai-completions".to_string());
    extension.base_url = Some("https://ext.test/v1".to_string());
    extension.models = Some(vec![extension_chat("ext"), extension_image("img")]);
    let base = base_provider(vec![AnyModel::Chat(chat("static"))], empty_auth());
    let provider =
        compose_model_provider("demo", Some(base), &config, Some(&extension)).expect("composed");
    let models = provider.get_all_models();
    let ids: Vec<&str> = models
        .iter()
        .map(|model| model.model().id.as_str())
        .collect();
    assert_eq!(ids, ["ext", "img"]);
    // 扩展定义的 chat 模型带上 chat 专有字段；image 保留 output。
    assert_eq!(models[0].model().api, "openai-completions");
    assert_eq!(models[0].model().base_url, "https://ext.test/v1");
    assert_eq!(models[0].model().context_window, Some(1000));
    assert_eq!(models[0].model().reasoning, Some(false));
    assert_eq!(models[1].model().model_type, Some(ModelType::Image));
    assert_eq!(models[1].model().output, Some(vec!["image".to_string()]));
}

#[tokio::test]
async fn extension_base_url_hard_overrides_without_models() {
    let (_dir, config) = load_config(json!({"providers": {}})).await;
    let mut extension = provider_input();
    extension.api_key = Some("k".to_string());
    extension.base_url = Some("https://ext.test/v1".to_string());
    let base = base_provider(
        vec![
            AnyModel::Chat(chat("static")),
            AnyModel::Image(image("img")),
        ],
        empty_auth(),
    );
    let provider =
        compose_model_provider("demo", Some(base), &config, Some(&extension)).expect("composed");
    let models = provider.get_all_models();
    assert_eq!(models.len(), 2);
    assert!(
        models
            .iter()
            .all(|model| model.model().base_url == "https://ext.test/v1")
    );
}

#[tokio::test]
async fn extension_classifier_keeps_its_context_window() {
    let (_dir, config) = load_config(json!({"providers": {}})).await;
    let mut extension = provider_input();
    extension.api_key = Some("k".to_string());
    extension.api = Some("typesafe-system-one".to_string());
    extension.base_url = Some("https://ext.test/v1".to_string());
    extension.models = Some(vec![extension_classifier("cls")]);
    let provider =
        compose_model_provider("demo", None, &config, Some(&extension)).expect("composed");
    let models = provider.get_all_models();
    assert_eq!(models[0].model().model_type, Some(ModelType::Classifier));
    assert_eq!(models[0].model().context_window, Some(64_000));
}

#[tokio::test]
async fn model_overrides_apply_last_and_only_to_chat_models() {
    let (_dir, config) = load_config(json!({
        "providers": {"demo": {"apiKey": "k", "modelOverrides": {"x": {"name": "Overridden", "contextWindow": 2048}}}}
    }))
    .await;
    let mut extension = provider_input();
    extension.api = Some("openai-completions".to_string());
    extension.base_url = Some("http://ext.test".to_string());
    extension.models = Some(vec![extension_chat("x"), extension_image("x")]);
    let provider =
        compose_model_provider("demo", None, &config, Some(&extension)).expect("composed");
    let models = provider.get_all_models();
    assert_eq!(models[0].model().name, "Overridden");
    assert_eq!(models[0].model().context_window, Some(2048));
    assert_eq!(models[1].model().name, "x");
}

// ---------------------------------------------------------------------------
// auth 组合
// ---------------------------------------------------------------------------

#[tokio::test]
async fn api_key_login_prompts_with_the_verbatim_message() {
    let (_dir, config) = load_config(json!({
        "providers": {"demo": {"baseUrl": "http://x", "apiKey": "literal"}}
    }))
    .await;
    let provider = compose_model_provider("demo", None, &config, None).expect("composed");
    let auth = provider.auth().api_key.clone().expect("api key auth");
    assert_eq!(auth.name(), "API key");
    let interaction = RecordingInteraction::new(vec!["typed-key"]);
    let credential = auth.login(&interaction).await.expect("login");
    assert_eq!(credential.key.as_deref(), Some("typed-key"));
    assert_eq!(interaction.prompt_types(), ["secret"]);
    assert_eq!(interaction.messages(), ["Enter API key"]);
}

#[tokio::test]
async fn literal_api_key_resolves_and_checks() {
    let (_dir, config) = load_config(json!({
        "providers": {"demo": {"baseUrl": "http://x", "apiKey": "literal"}}
    }))
    .await;
    let provider = compose_model_provider("demo", None, &config, None).expect("composed");
    let auth = provider.auth().api_key.clone().expect("api key auth");
    let ctx = TestAuthContext::default();
    let signal = AbortScope::new();
    let check = auth
        .check(ApiKeyAuthInput {
            ctx: &ctx,
            credential: None,
            signal: &signal,
        })
        .await
        .expect("check")
        .expect("configured");
    assert_eq!(check.source.as_deref(), Some("configured API key"));
    let result = auth
        .resolve(ApiKeyAuthInput {
            ctx: &ctx,
            credential: None,
            signal: &signal,
        })
        .await
        .expect("resolve")
        .expect("result");
    assert_eq!(result.auth.api_key.as_deref(), Some("literal"));
    assert_eq!(result.source.as_deref(), Some("configured API key"));
}

#[tokio::test]
async fn environment_api_key_interpolation() {
    let (_dir, config) = load_config(json!({
        "providers": {"demo": {"baseUrl": "http://x", "apiKey": "$DEMO_KEY"}}
    }))
    .await;
    let provider = compose_model_provider("demo", None, &config, None).expect("composed");
    let auth = provider.auth().api_key.clone().expect("api key auth");
    let mut ctx = TestAuthContext::default();
    let signal = AbortScope::new();
    assert!(
        auth.check(ApiKeyAuthInput {
            ctx: &ctx,
            credential: None,
            signal: &signal
        })
        .await
        .unwrap()
        .is_none()
    );
    let error = auth
        .resolve(ApiKeyAuthInput {
            ctx: &ctx,
            credential: None,
            signal: &signal,
        })
        .await
        .expect_err("missing env");
    assert_eq!(
        error.message,
        "Failed to resolve API key for provider \"demo\" from environment variable: DEMO_KEY"
    );
    ctx.env
        .insert("DEMO_KEY".to_string(), "from-env".to_string());
    let result = auth
        .resolve(ApiKeyAuthInput {
            ctx: &ctx,
            credential: None,
            signal: &signal,
        })
        .await
        .expect("resolve")
        .expect("result");
    assert_eq!(result.auth.api_key.as_deref(), Some("from-env"));
    assert!(
        auth.check(ApiKeyAuthInput {
            ctx: &ctx,
            credential: None,
            signal: &signal
        })
        .await
        .unwrap()
        .is_some()
    );
}

#[tokio::test]
async fn multiple_missing_environment_variables_use_the_plural_message() {
    let (_dir, config) = load_config(json!({
        "providers": {"demo": {"baseUrl": "http://x", "apiKey": "$A_KEY-$B_KEY"}}
    }))
    .await;
    let provider = compose_model_provider("demo", None, &config, None).expect("composed");
    let auth = provider.auth().api_key.clone().expect("api key auth");
    let ctx = TestAuthContext::default();
    let signal = AbortScope::new();
    let error = auth
        .resolve(ApiKeyAuthInput {
            ctx: &ctx,
            credential: None,
            signal: &signal,
        })
        .await
        .expect_err("missing env");
    assert_eq!(
        error.message,
        "Failed to resolve API key for provider \"demo\" from environment variables: A_KEY, B_KEY"
    );
}

#[tokio::test]
async fn command_api_key_failure_and_cache() {
    clear_api_key_cache();
    let (_dir, config) = load_config(json!({
        "providers": {"demo": {"baseUrl": "http://x", "apiKey": "!exit 1"}}
    }))
    .await;
    let provider = compose_model_provider("demo", None, &config, None).expect("composed");
    let auth = provider.auth().api_key.clone().expect("api key auth");
    let ctx = TestAuthContext::default();
    let signal = AbortScope::new();
    let check = auth
        .check(ApiKeyAuthInput {
            ctx: &ctx,
            credential: None,
            signal: &signal,
        })
        .await
        .unwrap()
        .expect("commands are reported as configured");
    assert_eq!(check.source.as_deref(), Some("configured API key"));
    let error = auth
        .resolve(ApiKeyAuthInput {
            ctx: &ctx,
            credential: None,
            signal: &signal,
        })
        .await
        .expect_err("command fails");
    assert_eq!(
        error.message,
        "Failed to resolve API key for provider \"demo\" from shell command: exit 1"
    );
    clear_api_key_cache();
}

#[tokio::test]
async fn inherited_auth_is_delegated_and_renamed() {
    let (_dir, config) = load_config(json!({
        "providers": {"demo": {"baseUrl": "http://x", "apiKey": "config-key"}}
    }))
    .await;
    let base = base_provider(
        vec![AnyModel::Chat(chat("static"))],
        ProviderAuth {
            api_key: Some(Arc::new(InheritedAuth)),
            oauth: None,
        },
    );
    let provider = compose_model_provider("demo", Some(base), &config, None).expect("composed");
    let auth = provider.auth().api_key.clone().expect("api key auth");
    assert_eq!(auth.name(), "Inherited key");
    let ctx = TestAuthContext::default();
    let signal = AbortScope::new();
    // 有 credential 时直接委托 inherited.check。
    let check = auth
        .check(ApiKeyAuthInput {
            ctx: &ctx,
            credential: Some(&ApiKeyCredential {
                key: Some("stored".to_string()),
                env: None,
                extra: IndexMap::new(),
            }),
            signal: &signal,
        })
        .await
        .unwrap()
        .expect("check");
    assert_eq!(check.source.as_deref(), Some("inherited check"));
    // 无 credential 且有 rawKey 时，继承 resolve 收到解析后的 key。
    let result = auth
        .resolve(ApiKeyAuthInput {
            ctx: &ctx,
            credential: None,
            signal: &signal,
        })
        .await
        .unwrap()
        .expect("result");
    assert_eq!(result.auth.api_key.as_deref(), Some("config-key"));
    assert_eq!(result.source.as_deref(), Some("inherited source"));
}

#[tokio::test]
async fn stored_credential_is_used_when_there_is_no_inherited_auth() {
    let (_dir, config) = load_config(json!({
        "providers": {"demo": {"baseUrl": "http://x"}}
    }))
    .await;
    let base = base_provider(vec![AnyModel::Chat(chat("static"))], no_auth());
    let mut extension = provider_input();
    extension.api_key = Some("ext-key".to_string());
    let provider =
        compose_model_provider("demo", Some(base), &config, Some(&extension)).expect("composed");
    let auth = provider.auth().api_key.clone().expect("api key auth");
    let ctx = TestAuthContext::default();
    let signal = AbortScope::new();
    let credential = ApiKeyCredential {
        key: Some("stored-key".to_string()),
        env: Some(ProviderEnv::from([(
            "EXTRA".to_string(),
            "value".to_string(),
        )])),
        extra: IndexMap::new(),
    };
    let check = auth
        .check(ApiKeyAuthInput {
            ctx: &ctx,
            credential: Some(&credential),
            signal: &signal,
        })
        .await
        .unwrap()
        .expect("check");
    assert_eq!(check.source.as_deref(), Some("stored credential"));
    let result = auth
        .resolve(ApiKeyAuthInput {
            ctx: &ctx,
            credential: Some(&credential),
            signal: &signal,
        })
        .await
        .unwrap()
        .expect("result");
    assert_eq!(result.auth.api_key.as_deref(), Some("stored-key"));
    assert_eq!(result.env, credential.env);
    assert_eq!(result.source.as_deref(), Some("stored credential"));
}

#[tokio::test]
async fn oauth_only_providers_have_no_api_key_auth() {
    let (_dir, config) = load_config(json!({"providers": {}})).await;
    let base = base_provider(
        vec![AnyModel::Chat(chat("static"))],
        ProviderAuth {
            api_key: None,
            oauth: Some(Arc::new(StaticOAuth {
                name: "Demo OAuth".to_string(),
            })),
        },
    );
    let provider = compose_model_provider("demo", Some(base), &config, None).expect("composed");
    assert!(provider.auth().api_key.is_none());
    assert!(provider.auth().oauth.is_some());
    assert_eq!(provider.auth().oauth.as_ref().unwrap().name(), "Demo OAuth");
}

#[tokio::test]
async fn configured_headers_are_resolved_and_auth_header_is_added() {
    let (_dir, config) = load_config(json!({
        "providers": {"demo": {"baseUrl": "http://x", "apiKey": "k",
            "authHeader": true, "headers": {"X-Extra": "$HEADER_ENV"}}}
    }))
    .await;
    let provider = compose_model_provider("demo", None, &config, None).expect("composed");
    let auth = provider.auth().api_key.clone().expect("api key auth");
    let mut ctx = TestAuthContext::default();
    ctx.env
        .insert("HEADER_ENV".to_string(), "header-value".to_string());
    let signal = AbortScope::new();
    let result = auth
        .resolve(ApiKeyAuthInput {
            ctx: &ctx,
            credential: None,
            signal: &signal,
        })
        .await
        .unwrap()
        .expect("result");
    let headers = result.auth.headers.expect("headers");
    assert_eq!(
        headers.get("X-Extra"),
        Some(&Some("header-value".to_string()))
    );
    assert_eq!(
        headers.get("Authorization"),
        Some(&Some("Bearer k".to_string()))
    );
}

#[tokio::test]
async fn auth_header_requires_a_resolved_api_key() {
    let (_dir, config) = load_config(json!({
        "providers": {"demo": {"baseUrl": "http://x", "authHeader": true}}
    }))
    .await;
    let base = base_provider(
        vec![AnyModel::Chat(chat("static"))],
        ProviderAuth {
            api_key: Some(Arc::new(StaticAuth {
                name: "Test".to_string(),
                result: Some(AuthResult::default()),
            })),
            oauth: None,
        },
    );
    let provider = compose_model_provider("demo", Some(base), &config, None).expect("composed");
    let auth = provider.auth().api_key.clone().expect("api key auth");
    let ctx = TestAuthContext::default();
    let signal = AbortScope::new();
    let error = auth
        .resolve(ApiKeyAuthInput {
            ctx: &ctx,
            credential: None,
            signal: &signal,
        })
        .await
        .expect_err("no api key");
    assert_eq!(error.message, "authHeader requires a resolved API key");
}

#[tokio::test]
async fn header_resolution_errors_name_the_provider_and_header() {
    let (_dir, config) = load_config(json!({
        "providers": {"demo": {"baseUrl": "http://x", "apiKey": "k", "headers": {"X-Thing": "$MISSING_HDR"}}}
    }))
    .await;
    let provider = compose_model_provider("demo", None, &config, None).expect("composed");
    let auth = provider.auth().api_key.clone().expect("api key auth");
    let ctx = TestAuthContext::default();
    let signal = AbortScope::new();
    let error = auth
        .resolve(ApiKeyAuthInput {
            ctx: &ctx,
            credential: None,
            signal: &signal,
        })
        .await
        .expect_err("missing header env");
    assert_eq!(
        error.message,
        "Failed to resolve provider \"demo\" header \"X-Thing\" from environment variable: MISSING_HDR"
    );
}

// ---------------------------------------------------------------------------
// OAuth 适配
// ---------------------------------------------------------------------------

fn extension_oauth(prompts: Arc<Mutex<Vec<String>>>) -> ExtensionOAuthConfig {
    let login: Arc<ExtensionLoginFn> = Arc::new(
        move |callbacks: &ExtensionOAuthCallbacks<'_>| -> ExtensionLoginFuture<'_> {
            let prompts = Arc::clone(&prompts);
            Box::pin(async move {
                callbacks.on_auth(OAuthAuthInfo {
                    url: "https://auth.test".to_string(),
                    instructions: Some("open it".to_string()),
                });
                callbacks.on_progress("waiting");
                callbacks.on_device_code(OAuthDeviceCodeInfo {
                    user_code: "ABCD".to_string(),
                    verification_uri: "https://device.test".to_string(),
                    open_browser_on_confirm: Some(false),
                    copy_code_on_confirm: None,
                    interval_seconds: Some(5.0),
                    expires_in_seconds: None,
                });
                let text = callbacks
                    .on_prompt(OAuthPrompt {
                        message: "Enter code".to_string(),
                        placeholder: Some("code".to_string()),
                        allow_empty: None,
                    })
                    .await?;
                prompts.lock().unwrap().push(text);
                let manual = callbacks.on_manual_code_input().await?;
                prompts.lock().unwrap().push(manual);
                let selected = callbacks
                    .on_select(OAuthSelectPrompt {
                        message: "Pick".to_string(),
                        options: vec![OAuthSelectOption {
                            id: "one".to_string(),
                            label: "One".to_string(),
                        }],
                    })
                    .await?;
                prompts.lock().unwrap().push(selected);
                assert!(!callbacks.signal().is_aborted());
                Ok(OAuthCredentials {
                    access: "access-token".to_string(),
                    refresh: "refresh-token".to_string(),
                    expires: 42.0,
                    extra: IndexMap::new(),
                })
            })
        },
    );
    let refresh: Arc<ExtensionRefreshTokenFn> = Arc::new(
        |credentials: OAuthCredentials,
         _signal: AbortScope|
         -> Pin<Box<dyn Future<Output = Result<OAuthCredentials, AuthError>> + Send>> {
            Box::pin(async move {
                Ok(OAuthCredentials {
                    access: format!("{}!", credentials.access),
                    ..credentials
                })
            })
        },
    );
    let get_api_key =
        Arc::new(|credentials: &OAuthCredentials| format!("key-{}", credentials.access));
    let modify_models: Arc<kiana_config::models::composer::ExtensionModifyModelsFn> =
        Arc::new(|models: Vec<Model>, _credentials: &OAuthCredentials| {
            models
                .into_iter()
                .map(|mut model| {
                    model.name = format!("modified-{}", model.name);
                    model
                })
                .collect()
        });
    ExtensionOAuthConfig {
        name: "Extension OAuth".to_string(),
        is_subscription: Some(true),
        uses_callback_server: None,
        login,
        refresh_token: refresh,
        get_api_key,
        modify_models: Some(modify_models),
    }
}

#[tokio::test]
async fn adapts_extension_oauth_callbacks_and_auth() {
    let (_dir, config) = load_config(json!({
        "providers": {"demo": {"apiKey": "k", "headers": {"X-Env": "$OAUTH_ENV"}}}
    }))
    .await;
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let extension = ProviderConfigInput {
        oauth: Some(extension_oauth(Arc::clone(&prompts))),
        ..ProviderConfigInput::default()
    };
    let provider =
        compose_model_provider("demo", None, &config, Some(&extension)).expect("composed");
    let oauth = provider.auth().oauth.clone().expect("oauth auth");
    assert_eq!(oauth.name(), "Extension OAuth");
    assert!(oauth.is_subscription());

    let interaction = RecordingInteraction::new(vec!["text-code", "manual-code", "one"]);
    let credential = oauth
        .login(
            &interaction,
            &kiana_ai::auth::types::LoginOptions::default(),
        )
        .await
        .expect("login");
    assert_eq!(credential.access, "access-token");
    assert_eq!(credential.refresh, "refresh-token");
    assert_eq!(credential.expires, 42.0);
    assert_eq!(
        prompts.lock().unwrap().clone(),
        ["text-code", "manual-code", "one"]
    );
    assert_eq!(
        interaction.prompt_types(),
        ["text", "manual_code", "select"]
    );
    assert_eq!(
        interaction.messages(),
        ["Enter code", "Paste the authorization code", "Pick"]
    );
    let events = interaction.events.lock().unwrap().clone();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].type_name(), "auth_url");
    assert_eq!(events[1].type_name(), "progress");
    assert_eq!(events[2].type_name(), "device_code");

    let refreshed = oauth
        .refresh(&credential, &AbortScope::new())
        .await
        .expect("refresh");
    assert_eq!(refreshed.access, "access-token!");

    // toAuth：getApiKey + 配置头解析（credential.env 存在 extra 里）+ authHeader。
    let mut credential_with_env = refreshed;
    credential_with_env
        .extra
        .insert("env".to_string(), json!({"OAUTH_ENV": "resolved"}));
    let auth = oauth.to_auth(&credential_with_env).await.expect("to auth");
    assert_eq!(auth.api_key.as_deref(), Some("key-access-token!"));
}

#[tokio::test]
async fn oauth_modify_models_runs_after_extension_models() {
    let (_dir, config) = load_config(json!({"providers": {}})).await;
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let extension = ProviderConfigInput {
        api_key: Some("k".to_string()),
        api: Some("openai-completions".to_string()),
        base_url: Some("http://ext.test".to_string()),
        models: Some(vec![extension_chat("chat"), extension_classifier("cls")]),
        oauth: Some(extension_oauth(Arc::clone(&prompts))),
        ..ProviderConfigInput::default()
    };
    let provider =
        compose_model_provider("demo", None, &config, Some(&extension)).expect("composed");
    assert_eq!(provider.get_models()[0].name, "chat");

    let published = Arc::new(AtomicUsize::new(0));
    let publish_counter = Arc::clone(&published);
    let publish: kiana_ai::models::PublishFn = Arc::new(move |publication: ModelsPublication| {
        publication.update.expect("update")();
        let counter = Arc::clone(&publish_counter);
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        })
    });
    provider
        .refresh_models(RefreshModelsContext {
            credential: Some(kiana_ai::auth::types::Credential::OAuth(
                OAuthCredential::new("access", "refresh", 1.0),
            )),
            stored: None,
            publish,
            allow_network: false,
            force: None,
            signal: AbortScope::new(),
            base_url: None,
        })
        .await
        .expect("refresh");
    assert_eq!(published.load(Ordering::SeqCst), 1);
    let models = provider.get_all_models();
    let ids: Vec<&str> = models
        .iter()
        .map(|model| model.model().id.as_str())
        .collect();
    // modifyModels 重排：chat 在前，非 chat 在后。
    assert_eq!(ids, ["chat", "cls"]);
    assert_eq!(models[0].model().name, "modified-chat");
    assert_eq!(models[1].model().name, "cls");
}

// ---------------------------------------------------------------------------
// 请求级头与状态
// ---------------------------------------------------------------------------

#[tokio::test]
async fn configured_model_headers_merge_overrides_and_extension_headers() {
    let (_dir, config) = load_config(json!({
        "providers": {"demo": {
            "apiKey": "k",
            "headers": {"P": "provider"},
            "models": [{"id": "m", "headers": {"A": "a", "P": "model"}}],
            "modelOverrides": {"m": {"headers": {"B": "$OVERRIDE_ENV"}}}
        }}
    }))
    .await;
    let provider = config.get_provider("demo").expect("provider");
    let mut extension = provider_input();
    extension.headers = Some(JsonHeaders::from([("C".to_string(), "c".to_string())]));
    extension.models = Some(vec![extension_chat("m")]);
    let model = AnyModel::Chat(chat("m"));
    let env = Some(IndexMap::from([(
        "OVERRIDE_ENV".to_string(),
        "override".to_string(),
    )]));
    let resolved =
        resolve_configured_model_headers(&model, Some(&provider), Some(&extension), env.as_ref())
            .unwrap()
            .expect("headers");
    assert_eq!(
        resolved.keys().cloned().collect::<Vec<_>>(),
        ["B", "A", "P"],
        "modelOverrides first, then models.json definition, then extension"
    );
    assert_eq!(resolved.get("B").map(String::as_str), Some("override"));
    assert_eq!(resolved.get("A").map(String::as_str), Some("a"));

    // 兼容配置：provider 头 + 模型头 + authHeader（该函数没有 env 参数，走进程环境）。
    // SAFETY: 变量名只在本测试里使用。
    unsafe { std::env::set_var("OVERRIDE_ENV", "override") };
    let mut with_auth_header = extension.clone();
    with_auth_header.auth_header = Some(true);
    let request =
        resolve_compatibility_request_config(&model, Some(&provider), Some(&with_auth_header))
            .expect("compat config");
    unsafe { std::env::remove_var("OVERRIDE_ENV") };
    assert!(request.auth_header);
    let headers = request.headers.expect("headers");
    assert_eq!(headers.get("A"), Some(&Some("a".to_string())));
    assert_eq!(headers.get("P"), Some(&Some("model".to_string())));
    assert_eq!(headers.get("C"), Some(&Some("c".to_string())));
}

#[tokio::test]
async fn compatibility_request_config_includes_model_static_headers() {
    let (_dir, _config) = load_config(json!({"providers": {}})).await;
    let mut model = chat("m");
    model.headers = Some(IndexMap::from([("Static".to_string(), "yes".to_string())]));
    let request =
        resolve_compatibility_request_config(&AnyModel::Chat(model.clone()), None, None).unwrap();
    // 没有配置头时，model.headers 仍然带出。
    let headers = request.headers.expect("headers");
    assert_eq!(headers.get("Static"), Some(&Some("yes".to_string())));
    assert_eq!(headers.get("A"), None);
}

#[tokio::test]
async fn configured_request_auth_status_sources() {
    let (_dir, config) = load_config(json!({
        "providers": {"demo": {"baseUrl": "http://x", "apiKey": "literal"}}
    }))
    .await;
    let provider = config.get_provider("demo").unwrap();
    let status = configured_request_auth_status(Some(&provider), None).unwrap();
    assert!(status.configured);
    assert_eq!(status.source, Some(AuthStatusSource::ModelsJsonKey));

    let extension = ProviderConfigInput {
        api_key: Some("literal".to_string()),
        ..ProviderConfigInput::default()
    };
    let status = configured_request_auth_status(Some(&provider), Some(&extension)).unwrap();
    assert_eq!(status.source, Some(AuthStatusSource::Fallback));

    let (_dir, config) = load_config(json!({
        "providers": {"demo": {"baseUrl": "http://x", "apiKey": "!echo hi"}}
    }))
    .await;
    let provider = config.get_provider("demo").unwrap();
    let status = configured_request_auth_status(Some(&provider), None).unwrap();
    assert_eq!(status.source, Some(AuthStatusSource::ModelsJsonCommand));

    let key = "KIANA_CONFIG_COMPOSER_ENV_TEST";
    let (_dir, config) = load_config(json!({
        "providers": {"demo": {"baseUrl": "http://x", "apiKey": "$KIANA_CONFIG_COMPOSER_ENV_TEST"}}
    }))
    .await;
    let provider = config.get_provider("demo").unwrap();
    let status = configured_request_auth_status(Some(&provider), None).unwrap();
    assert!(!status.configured);
    // SAFETY: 变量名只在本测试里使用（Rust 2024 的 set_var 是 unsafe 的）。
    unsafe { std::env::set_var(key, "set") };
    let status = configured_request_auth_status(Some(&provider), None).unwrap();
    assert!(status.configured);
    assert_eq!(status.source, Some(AuthStatusSource::Environment));
    assert_eq!(status.label.as_deref(), Some(key));
    unsafe { std::env::remove_var(key) };
}

// ---------------------------------------------------------------------------
// 短路与流分派
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_overlay_returns_the_base_provider_unchanged() {
    let (_dir, config) = load_config(json!({"providers": {}})).await;
    let base = base_provider(vec![AnyModel::Chat(chat("static"))], empty_auth());
    let provider =
        compose_model_provider("demo", Some(Arc::clone(&base)), &config, None).expect("composed");
    assert!(Arc::ptr_eq(&provider, &base));
}

#[tokio::test]
async fn no_overlay_without_a_base_still_composes() {
    // TS `composeModelProvider(id, undefined, config, undefined)`：没有 base 也没有 overlay 时
    // 不是错误，而是空模型列表 + 合成 API key 登录方式（`applyModelsJson` 在无 config 时
    // 直接返回 baseModels）。
    let (_dir, config) = load_config(json!({"providers": {}})).await;
    let provider = compose_model_provider("demo", None, &config, None).expect("composed");
    assert_eq!(provider.id(), "demo");
    assert!(provider.get_models().is_empty());
    assert_eq!(
        provider.auth().api_key.as_ref().map(|auth| auth.name()),
        Some("API key")
    );
}

#[tokio::test]
async fn extension_stream_simple_dispatches_for_its_api() {
    let (_dir, config) = load_config(json!({"providers": {}})).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let extension = ProviderConfigInput {
        api_key: Some("k".to_string()),
        api: Some("openai-completions".to_string()),
        stream_simple: Some(Arc::new(move |model, _, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            kiana_ai::api::lazy::lazy_stream_error(
                model,
                kiana_ai::AiError::new(kiana_ai::AiErrorKind::Other, "extension-stream"),
            )
        })),
        ..ProviderConfigInput::default()
    };
    let provider =
        compose_model_provider("demo", None, &config, Some(&extension)).expect("composed");
    let model = chat("m");
    let stream = provider.stream(
        &model,
        &TranscriptContext::default(),
        StreamOptions::default(),
    );
    let result = stream.result().await.expect("result");
    assert_eq!(result.error_message.as_deref(), Some("extension-stream"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn superseded_refresh_does_not_validate_refreshed_models() {
    // TS 在 publication 的 `update` 里校验刷新结果；publish 被取代（返回 false）时
    // update 不执行，刷新按成功返回。
    let (_dir, config) = load_config(json!({"providers": {}})).await;
    let mut base = model_base("chat");
    base.api = Some("openai-completions".to_string());
    base.base_url = Some("http://ext.test".to_string());
    let initial = ProviderModelConfig::Chat(ProviderChatModelConfig {
        base,
        reasoning: false,
        context_window: 1000.0,
        max_tokens: 100.0,
        ..ProviderChatModelConfig::default()
    });
    let refresh: Arc<kiana_config::models::composer::ExtensionRefreshModelsFn> =
        Arc::new(|_context| {
            Box::pin(async {
                // 没有 api / baseUrl，且 provider 级也没有：校验会失败。
                Ok(vec![extension_chat("broken")])
            })
        });
    let extension = ProviderConfigInput {
        api_key: Some("k".to_string()),
        models: Some(vec![initial]),
        refresh_models: Some(refresh),
        ..ProviderConfigInput::default()
    };
    let provider =
        compose_model_provider("demo", None, &config, Some(&extension)).expect("composed");
    let publish: kiana_ai::models::PublishFn =
        Arc::new(|_publication| Box::pin(async { Ok(false) }));
    provider
        .refresh_models(RefreshModelsContext {
            credential: Some(kiana_ai::auth::types::Credential::ApiKey(
                kiana_ai::auth::types::ApiKeyCredential {
                    key: Some("k".to_string()),
                    env: None,
                    extra: IndexMap::new(),
                },
            )),
            stored: None,
            publish,
            allow_network: false,
            force: None,
            signal: AbortScope::new(),
            base_url: None,
        })
        .await
        .expect("superseded refresh resolves without validating");
    assert_eq!(provider.get_all_models()[0].model().id, "chat");

    // 对照：update 真正执行时校验错误照常上抛。
    let updated = Arc::new(AtomicUsize::new(0));
    let updated_slot = Arc::clone(&updated);
    let publish: kiana_ai::models::PublishFn = Arc::new(move |publication: ModelsPublication| {
        publication.update.expect("update")();
        updated_slot.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(true) })
    });
    let error = provider
        .refresh_models(RefreshModelsContext {
            credential: Some(kiana_ai::auth::types::Credential::ApiKey(
                kiana_ai::auth::types::ApiKeyCredential {
                    key: Some("k".to_string()),
                    env: None,
                    extra: IndexMap::new(),
                },
            )),
            stored: None,
            publish,
            allow_network: false,
            force: None,
            signal: AbortScope::new(),
            base_url: None,
        })
        .await
        .expect_err("validated refresh fails");
    assert_eq!(
        error.message,
        "Provider demo, model broken: no \"api\" specified. Set it at model level or provider level."
    );
    assert_eq!(updated.load(Ordering::SeqCst), 1);
    // 校验失败时状态不变。
    assert_eq!(provider.get_all_models()[0].model().id, "chat");
}

#[tokio::test]
async fn missing_image_and_classifier_implementations_are_not_aborted() {
    // TS 的 imageErrorResult/classifierErrorResult 不传 aborted，默认 false。
    let (_dir, config) = load_config(json!({"providers": {}})).await;
    let extension = ProviderConfigInput {
        api_key: Some("k".to_string()),
        ..ProviderConfigInput::default()
    };
    let provider =
        compose_model_provider("demo", None, &config, Some(&extension)).expect("composed");
    let model = chat("m");
    let signal = tokio_util::sync::CancellationToken::new();
    signal.cancel();
    let options = StreamOptions {
        signal: Some(signal),
        ..StreamOptions::default()
    };
    let images = provider
        .generate_images(&model, &ImagesContext::default(), Some(&options))
        .await;
    assert_eq!(images.stop_reason, ImagesStopReason::Error);
    assert_eq!(
        images.error_message.as_deref(),
        Some("Provider demo has no image implementation for \"openai-completions\"")
    );
    let classifier = provider
        .classify(&model, &ClassifierContext::default(), Some(&options))
        .await;
    assert_eq!(classifier.stop_reason, ClassifierStopReason::Error);
    assert_eq!(
        classifier.error_message.as_deref(),
        Some("Provider demo has no classifier implementation for \"openai-completions\"")
    );
}

#[tokio::test]
async fn unknown_api_reports_the_verbatim_error() {
    let (_dir, config) = load_config(json!({
        "providers": {"demo": {"baseUrl": "http://x", "apiKey": "k",
            "models": [{"id": "m", "api": "totally-unknown-api"}]}}
    }))
    .await;
    let provider = compose_model_provider("demo", None, &config, None).expect("composed");
    let model = provider.get_models().remove(0);
    let stream = provider.stream(
        &model,
        &TranscriptContext::default(),
        StreamOptions::default(),
    );
    let result = stream.result().await.expect("result");
    assert_eq!(
        result.error_message.as_deref(),
        Some("No API provider registered for api: totally-unknown-api")
    );
}
