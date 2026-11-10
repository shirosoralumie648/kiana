//! `packages/ai/test/providers.test.ts` 的 Rust 移植。
//!
//! catalog/auth/provider-runtime 断言逐条对应；`compat.getModel/getModels`（compat 单元已合入）
//! 的未知 provider 空结果断言也在本文件内。

mod models_support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::api::lazy::{LazyApiCapabilities, LazyApiLoad, lazy_api};
use kiana_ai::auth::helpers::env_api_key_auth;
use kiana_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthContext, AuthError, AuthEvent,
    AuthInteraction, AuthPromptRequest, AuthResult, ModelAuth, ProviderAuth,
};
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::models::{
    CreateModelsOptions, CreateProviderOptions, GetAuthTarget, InMemoryModelsStore,
    ModelsDeferredCancelOptions, ModelsDeferredFetchOptions, ModelsRefreshOptions,
    ModelsRequestTransforms, ModelsSimpleStreamOptions, ModelsStore, ProviderApiImpl,
    ProviderStreams, TransformHeadersFn, create_models, create_provider,
    get_supported_thinking_levels,
};
use kiana_ai::providers::all::{
    builtin_models, builtin_providers, get_all_builtin_models, get_builtin_classifier_model,
    get_builtin_classifier_models, get_builtin_image_model, get_builtin_image_models,
    get_builtin_model, get_builtin_models, get_builtin_providers,
};
use kiana_ai::providers::amazon_bedrock::amazon_bedrock_provider;
use kiana_ai::providers::anthropic::anthropic_provider;
use kiana_ai::providers::cloudflare_ai_gateway::cloudflare_ai_gateway_provider;
use kiana_ai::providers::cloudflare_workers_ai::cloudflare_workers_ai_provider;
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxDeferredOptions, FauxResponseStep,
    RegisterFauxProviderOptions, faux_assistant_message, faux_provider, faux_provider_with,
};
use kiana_ai::providers::google_vertex::google_vertex_provider;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessageEvent, Context, DeferredFetchOptions, DeferredHandle,
    DeferredRequest, DeferredWindow, DoneReason, Message, Model, ModelCost, ModelImageInputLimits,
    ModelImageResizeOptions, ModelInputLimits, ModelInputModality, ModelThinkingLevel,
    SimpleStreamOptions, StreamOptions, TranscriptContext, UserContent, UserMessage, UserRole,
};
use kiana_ai::utils::abort::AbortScope;
use kiana_ai::utils::diagnostics::ThrownValue;
use kiana_ai::utils::transcript::normalize_context;
use models_support::{ambient_auth, done_message, test_model};

fn plain_context() -> Context {
    Context {
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("hi".into()),
            timestamp: 0,
        })],
        ..Default::default()
    }
}

fn user_context() -> TranscriptContext {
    normalize_context(&Context {
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("hi".into()),
            timestamp: 0,
        })],
        ..Default::default()
    })
}

fn fake_auth_context(env: &[(&str, &str)], files: &[&str]) -> Arc<FakeAuthContext> {
    Arc::new(FakeAuthContext {
        env: env
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect(),
        files: files.iter().map(|path| (*path).to_string()).collect(),
    })
}

struct FakeAuthContext {
    env: IndexMap<String, String>,
    files: Vec<String>,
}

#[async_trait]
impl AuthContext for FakeAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        self.env.get(name).cloned()
    }
    async fn file_exists(&self, path: &str) -> bool {
        self.files.iter().any(|file| file == path)
    }
}

fn default_resize() -> ModelImageResizeOptions {
    ModelImageResizeOptions {
        max_width: Some(2000),
        max_height: Some(2000),
        max_bytes: Some(4_718_592),
        jpeg_quality: Some(80),
    }
}

fn compat(model: &Model) -> Option<&serde_json::Value> {
    model.compat.as_ref()
}

fn compat_has(model: &Model, key: &str) -> bool {
    compat(model).and_then(|compat| compat.get(key)).is_some()
}

fn compat_bool(model: &Model, key: &str) -> Option<bool> {
    compat(model)
        .and_then(|compat| compat.get(key))
        .and_then(serde_json::Value::as_bool)
}

fn deepseek_costs() -> (ModelCost, ModelCost) {
    (
        ModelCost {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write: 0.0,
            ..ModelCost::default()
        },
        ModelCost {
            input: 1.9,
            output: 8.0,
            cache_read: 0.38,
            cache_write: 0.0,
            ..ModelCost::default()
        },
    )
}

#[tokio::test]
async fn builtin_models_registers_every_builtin_provider_with_models() {
    let models = builtin_models(None);
    let providers = models.get_providers();
    assert_eq!(providers.len(), builtin_providers().len());
    assert!(
        providers
            .iter()
            .any(|provider| provider.id() == "anthropic")
    );

    let anthropic = models
        .get_model("anthropic", "claude-haiku-4-5")
        .expect("claude-haiku-4-5");
    assert_eq!(anthropic.api, "anthropic-messages");

    let all = models.get_models(None);
    assert!(all.len() > 500, "{}", all.len());

    for provider in providers {
        let list = models.get_all_models(Some(provider.id()));
        assert!(!list.is_empty(), "{}", provider.id());
        assert!(
            list.iter()
                .all(|model| model.model().provider == provider.id())
        );
    }
    let balanced = get_builtin_model("radius", "balanced").expect("radius balanced");
    assert_eq!(balanced.api, "kiana-messages");
    assert_eq!(balanced.provider, "radius");
}

/// TS 的 `getBuiltinModel`/`getBuiltinModels` 返回目录原始对象，因此 radius 数据里的
/// `lab`/`enabled`/`providers`（以及 `recursive/flint` 的 `free`）对调用方可见。Rust 的
/// [`Model`] 是冻结的固定结构体，表达不了这些键；catalog 层（`providers::catalog`）
/// 仍逐字保留原始 JSON，`ModelsStoreEntry::raw_models` 依赖它做无损往返。本测试锁定
/// 数据与 catalog 层不丢键，同时记录类型化视图的限制（见 `all.rs`/`radius_config.rs`）。
#[test]
fn keeps_unknown_radius_catalog_keys_in_the_raw_catalog_layer() {
    let raw = kiana_ai::providers::catalog::get_model("radius", "balanced").expect("raw balanced");
    assert_eq!(raw["lab"], serde_json::json!("Moonshot AI"));
    assert_eq!(raw["enabled"], serde_json::json!(true));
    assert_eq!(
        raw["providers"].as_array().map(|providers| providers.len()),
        Some(4)
    );
    let flint =
        kiana_ai::providers::catalog::get_model("radius", "recursive/flint").expect("flint");
    assert_eq!(flint["free"], serde_json::json!(true));

    let typed = get_builtin_model("radius", "balanced").expect("typed balanced");
    assert_eq!(typed.api, "kiana-messages");
    let serialized = serde_json::to_value(&typed).expect("serialize model");
    assert!(serialized.get("lab").is_none());
    assert!(serialized.get("enabled").is_none());
    assert!(serialized.get("providers").is_none());
}

#[test]
fn returns_empty_results_for_unknown_provider_ids() {
    let unknown = "not-a-provider";
    assert!(get_builtin_model(unknown, "x").is_none());
    assert!(get_builtin_image_model(unknown, "x").is_none());
    assert!(get_builtin_classifier_model(unknown, "x").is_none());
    assert!(get_builtin_models(unknown).is_empty());
    assert!(get_builtin_image_models(unknown).is_empty());
    assert!(get_builtin_classifier_models(unknown).is_empty());
    assert!(get_all_builtin_models(unknown).is_empty());
    assert!(kiana_ai::compat::get_model(unknown, "x").is_none());
    assert!(kiana_ai::compat::get_models(unknown).is_empty());
}

#[test]
fn stores_native_constrained_sampling_capabilities_in_model_metadata() {
    let gpt4o = get_builtin_model("openai", "gpt-4o").expect("gpt-4o");
    assert_eq!(compat_bool(&gpt4o, "supportsStrictMode"), Some(true));
    assert!(!compat_has(&gpt4o, "supportsOpenAIGrammarTools"));
    let gpt = get_builtin_model("openai", "gpt-5.4").expect("gpt-5.4");
    assert_eq!(compat_bool(&gpt, "supportsStrictMode"), Some(true));
    assert_eq!(compat_bool(&gpt, "supportsOpenAIGrammarTools"), Some(true));
    let haiku = get_builtin_model("anthropic", "claude-haiku-4-5").expect("haiku");
    assert_eq!(compat_bool(&haiku, "supportsStrictTools"), Some(true));
}

#[test]
fn keeps_the_conservative_resize_profile_on_every_vision_model() {
    let vision_models: Vec<Model> = get_builtin_providers()
        .iter()
        .flat_map(|provider| get_builtin_models(provider))
        .filter(|model| model.input.contains(&ModelInputModality::Image))
        .collect();
    assert!(!vision_models.is_empty());
    for model in vision_models {
        let resize = model
            .input_limits
            .as_ref()
            .and_then(|limits| limits.images.as_ref())
            .and_then(|images| images.resize.clone())
            .unwrap_or_else(|| panic!("{} missing resize", model.id));
        assert_eq!(resize, default_resize(), "{}", model.id);
    }
}

#[test]
fn records_known_direct_provider_image_request_limits() {
    let haiku = get_builtin_model("anthropic", "claude-haiku-4-5").expect("haiku");
    let limits = haiku.input_limits.expect("limits");
    assert_eq!(limits.max_request_bytes, Some(32 * 1024 * 1024));
    assert_eq!(
        limits
            .images
            .as_ref()
            .and_then(|images| images.max_per_request),
        Some(100)
    );
    assert_eq!(
        get_builtin_model("anthropic", "claude-opus-5")
            .expect("opus")
            .input_limits
            .and_then(|limits| limits.images)
            .and_then(|images| images.max_per_request),
        Some(600)
    );
    assert_eq!(
        get_builtin_model("amazon-bedrock", "anthropic.claude-haiku-4-5-20251001-v1:0")
            .expect("bedrock haiku")
            .input_limits
            .and_then(|limits| limits.images)
            .and_then(|images| images.max_per_message),
        Some(20)
    );
    let gpt4o = get_builtin_model("openai", "gpt-4o").expect("gpt-4o");
    let gpt_limits = gpt4o.input_limits.expect("limits");
    assert_eq!(gpt_limits.max_request_bytes, Some(512 * 1024 * 1024));
    assert_eq!(
        gpt_limits.images.and_then(|images| images.max_per_request),
        Some(1500)
    );
    let gemini = get_builtin_model("google", "gemini-2.5-flash").expect("gemini");
    let gemini_limits = gemini.input_limits.expect("limits");
    assert_eq!(gemini_limits.max_request_bytes, Some(20 * 1024 * 1024));
    assert_eq!(
        gemini_limits
            .images
            .and_then(|images| images.max_per_request),
        Some(3600)
    );
}

#[test]
fn does_not_infer_image_limits_from_gateway_api_compatibility() {
    let open_router_model = get_builtin_models("openrouter")
        .into_iter()
        .find(|model| model.input.contains(&ModelInputModality::Image))
        .expect("openrouter vision model");
    assert_eq!(
        open_router_model.input_limits,
        Some(ModelInputLimits {
            max_request_bytes: None,
            images: Some(ModelImageInputLimits {
                resize: Some(default_resize()),
                max_per_message: None,
                max_per_request: None,
            }),
        })
    );
}

#[test]
fn uses_models_dev_effort_levels_for_google_thinking_models() {
    for provider in ["google", "google-vertex"] {
        let flash36 = get_builtin_model(provider, "gemini-3.6-flash").expect("3.6");
        assert!(get_supported_thinking_levels(&flash36).contains(&ModelThinkingLevel::Minimal));
        assert_eq!(
            get_supported_thinking_levels(
                &get_builtin_model(provider, "gemini-3.8-flash").expect("3.8")
            ),
            vec![
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High
            ]
        );
        assert_eq!(
            get_supported_thinking_levels(
                &get_builtin_model(provider, "gemini-3.1-pro-preview").expect("3.1")
            ),
            vec![
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High
            ]
        );
    }
    assert_eq!(
        get_supported_thinking_levels(
            &get_builtin_model("opencode", "gemini-3.8-flash").expect("opencode 3.8")
        ),
        vec![
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High
        ]
    );
    assert_eq!(
        get_supported_thinking_levels(
            &get_builtin_model("google", "gemma-4-31b-it").expect("gemma")
        ),
        vec![ModelThinkingLevel::Minimal, ModelThinkingLevel::High]
    );
}

#[test]
fn enables_mid_conversation_system_messages_only_for_verified_models() {
    let models = builtin_models(None);
    let supported = [
        ("moonshotai", "kimi-k2.6"),
        ("moonshotai", "kimi-k2.7-code"),
        ("moonshotai", "kimi-k2.7-code-highspeed"),
        ("moonshotai", "kimi-k3"),
        ("moonshotai-cn", "kimi-k2.6"),
        ("moonshotai-cn", "kimi-k2.7-code"),
        ("moonshotai-cn", "kimi-k2.7-code-highspeed"),
        ("moonshotai-cn", "kimi-k3"),
        ("fireworks", "accounts/fireworks/models/kimi-k3"),
        ("fireworks", "accounts/fireworks/routers/kimi-k3-fast"),
        ("openai", "gpt-5.4"),
        ("openai", "gpt-5.5"),
        ("openai", "gpt-6-astra"),
        ("openai-codex", "gpt-5.6-sol"),
        ("anthropic", "claude-opus-5"),
        ("opencode", "gpt-5.4"),
        ("opencode", "gpt-5.6-terra"),
        ("opencode-go", "gpt-5.6-luna"),
        ("opencode", "claude-opus-4-8"),
        ("opencode", "claude-opus-5"),
        ("opencode", "kimi-k3"),
        ("opencode-go", "kimi-k3"),
        ("github-copilot", "gpt-5.6-terra"),
        ("github-copilot", "claude-opus-5"),
        ("github-copilot", "claude-opus-4.8"),
        ("github-copilot", "kimi-k3"),
        ("deepseek", "deepseek-v4-pro"),
        ("openrouter", "openai/gpt-5.6-terra"),
    ];
    let unsupported = [
        (
            "fireworks",
            "accounts/fireworks/models/nemotron-3-ultra-nvfp4",
        ),
        ("openai", "gpt-4.1"),
        ("openai", "gpt-5.2"),
        ("anthropic", "claude-sonnet-4-5"),
        ("google", "gemini-2.5-pro"),
        ("opencode", "gpt-5.2"),
        ("opencode", "claude-sonnet-4-5"),
        ("github-copilot", "claude-sonnet-4.6"),
        ("deepseek", "deepseek-flash"),
        ("openrouter", "anthropic/claude-opus-5"),
        ("openrouter", "moonshotai/kimi-k3"),
        ("openrouter", "openai/gpt-5.6-terra:batch"),
    ];
    for (provider, model_id) in supported {
        let model = models
            .get_model(provider, model_id)
            .unwrap_or_else(|| panic!("{provider}/{model_id}"));
        assert_eq!(
            compat_bool(&model, "supportsMidConvoSystemMessages"),
            Some(true),
            "{provider}/{model_id}"
        );
    }
    for (provider, model_id) in unsupported {
        let model = models
            .get_model(provider, model_id)
            .unwrap_or_else(|| panic!("{provider}/{model_id}"));
        assert!(
            !compat_has(&model, "supportsMidConvoSystemMessages"),
            "{provider}/{model_id}"
        );
    }
}

#[test]
fn routes_proxied_tool_changes_through_verified_transports_only() {
    let models = builtin_models(None);
    for (provider, model_id) in [
        ("opencode", "gpt-5.6-terra"),
        ("github-copilot", "gpt-5.6-terra"),
    ] {
        let model = models.get_model(provider, model_id).expect("gpt model");
        assert_eq!(
            compat_bool(&model, "supportsAdditionalTools"),
            Some(true),
            "{provider}/{model_id}"
        );
        assert!(
            !compat_has(&model, "supportsToolSearch"),
            "{provider}/{model_id}"
        );
    }
    for provider in ["opencode", "github-copilot"] {
        let model = models.get_model(provider, "claude-opus-5").expect("opus");
        assert!(
            !compat_has(&model, "supportsMidConvoToolChanges"),
            "{provider}"
        );
    }
    assert_eq!(
        compat_bool(
            &models
                .get_model("anthropic", "claude-opus-5")
                .expect("anthropic opus"),
            "supportsMidConvoToolChanges"
        ),
        Some(true)
    );
    for provider in ["moonshotai", "moonshotai-cn", "opencode", "opencode-go"] {
        let model = models.get_model(provider, "kimi-k3").expect("kimi-k3");
        assert_eq!(
            compat_bool(&model, "supportsMidConvoToolAdditions"),
            Some(true),
            "{provider}"
        );
    }
    for provider in ["moonshotai", "moonshotai-cn"] {
        for model_id in ["kimi-k2.6", "kimi-k2.7-code", "kimi-k2.7-code-highspeed"] {
            let model = models.get_model(provider, model_id).expect("kimi model");
            assert!(
                !compat_has(&model, "supportsMidConvoToolAdditions"),
                "{provider}/{model_id}"
            );
        }
    }
    assert!(!compat_has(
        &models
            .get_model("github-copilot", "kimi-k3")
            .expect("copilot kimi"),
        "supportsMidConvoToolAdditions"
    ));
    assert!(!compat_has(
        &models
            .get_model("openrouter", "openai/gpt-5.6-terra")
            .expect("openrouter gpt"),
        "supportsMidConvoToolAdditions"
    ));
}

#[test]
fn uses_official_kimi_k3_pricing_for_moonshot_providers() {
    let models = builtin_models(None);
    let (k3_cost, _) = deepseek_costs();
    for provider in ["moonshotai", "moonshotai-cn"] {
        assert_eq!(
            models.get_model(provider, "kimi-k3").expect("k3").cost,
            k3_cost,
            "{provider}"
        );
    }
}

#[test]
fn uses_api_equivalent_implied_pricing_for_kimi_coding_subscription_models() {
    let models = builtin_models(None);
    let (k3_cost, highspeed_cost) = deepseek_costs();
    assert_eq!(
        models.get_model("kimi-coding", "k3").expect("k3").cost,
        k3_cost
    );
    assert_eq!(
        models
            .get_model("kimi-coding", "kimi-for-coding-highspeed")
            .expect("highspeed")
            .cost,
        highspeed_cost
    );
}

#[tokio::test]
async fn resolves_anthropic_bearer_auth_from_env_with_auth_token_precedence() {
    let models = create_models(CreateModelsOptions {
        auth_context: Some(fake_auth_context(
            &[
                ("ANTHROPIC_AUTH_TOKEN", "auth-token"),
                ("ANTHROPIC_OAUTH_TOKEN", "oauth-token"),
                ("ANTHROPIC_API_KEY", "api-key"),
            ],
            &[],
        )),
        ..Default::default()
    });
    models.set_provider(anthropic_provider());

    let result = models
        .get_auth(GetAuthTarget::Provider("anthropic"), Default::default())
        .await
        .expect("auth")
        .expect("configured");
    assert_eq!(
        result.auth.headers,
        Some(
            [(
                "Authorization".to_string(),
                Some("Bearer auth-token".to_string())
            )]
            .into_iter()
            .collect()
        )
    );
    assert!(result.auth.api_key.is_none());
    assert_eq!(result.source.as_deref(), Some("ANTHROPIC_AUTH_TOKEN"));
}

#[tokio::test]
async fn preserves_anthropic_oauth_token_precedence_over_the_api_key() {
    let models = create_models(CreateModelsOptions {
        auth_context: Some(fake_auth_context(
            &[
                ("ANTHROPIC_API_KEY", "key"),
                ("ANTHROPIC_OAUTH_TOKEN", "oauth-token"),
            ],
            &[],
        )),
        ..Default::default()
    });
    models.set_provider(anthropic_provider());

    let result = models
        .get_auth(GetAuthTarget::Provider("anthropic"), Default::default())
        .await
        .expect("auth")
        .expect("configured");
    assert_eq!(result.auth.api_key.as_deref(), Some("oauth-token"));
    assert_eq!(result.source.as_deref(), Some("ANTHROPIC_OAUTH_TOKEN"));
}

/// TS `if (credential?.key)` 按真值判断：存储的 key 为空字符串时视为未配置，继续走
/// `ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_OAUTH_TOKEN` / `ANTHROPIC_API_KEY` / federation 回退。
#[tokio::test]
async fn ignores_an_empty_stored_anthropic_key_and_falls_back_to_env() {
    let auth = anthropic_provider()
        .auth()
        .api_key
        .clone()
        .expect("anthropic api key auth");
    let empty_credential = ApiKeyCredential {
        key: Some(String::new()),
        ..Default::default()
    };
    let signal = stub_signal();

    let ctx = fake_auth_context(&[("ANTHROPIC_API_KEY", "env-key")], &[]);
    let resolved = auth
        .resolve(ApiKeyAuthInput {
            ctx: ctx.as_ref(),
            credential: Some(&empty_credential),
            signal: &signal,
        })
        .await
        .expect("resolve")
        .expect("configured");
    assert_eq!(resolved.auth.api_key.as_deref(), Some("env-key"));
    assert_eq!(resolved.source.as_deref(), Some("ANTHROPIC_API_KEY"));

    let ctx = fake_auth_context(&[("ANTHROPIC_AUTH_TOKEN", "auth-token")], &[]);
    let resolved = auth
        .resolve(ApiKeyAuthInput {
            ctx: ctx.as_ref(),
            credential: Some(&empty_credential),
            signal: &signal,
        })
        .await
        .expect("resolve")
        .expect("configured");
    assert_eq!(resolved.source.as_deref(), Some("ANTHROPIC_AUTH_TOKEN"));

    // 空 key 也不会被当成已配置：没有任何 env/federation 时 resolve 返回 undefined。
    let ctx = fake_auth_context(&[], &[]);
    assert!(
        auth.resolve(ApiKeyAuthInput {
            ctx: ctx.as_ref(),
            credential: Some(&empty_credential),
            signal: &signal,
        })
        .await
        .expect("resolve")
        .is_none()
    );
}

/// TS `if (credential?.key)`：空的 Bedrock bearer token 不短路，继续探测 AWS 环境凭证。
#[tokio::test]
async fn ignores_an_empty_stored_bedrock_key_and_falls_back_to_aws_credentials() {
    let auth = amazon_bedrock_provider()
        .auth()
        .api_key
        .clone()
        .expect("bedrock api key auth");
    let empty_credential = ApiKeyCredential {
        key: Some(String::new()),
        ..Default::default()
    };
    let signal = stub_signal();

    let ctx = fake_auth_context(&[("AWS_PROFILE", "dev")], &[]);
    let resolved = auth
        .resolve(ApiKeyAuthInput {
            ctx: ctx.as_ref(),
            credential: Some(&empty_credential),
            signal: &signal,
        })
        .await
        .expect("resolve")
        .expect("configured");
    assert_eq!(resolved.auth, ModelAuth::default());
    assert_eq!(resolved.source.as_deref(), Some("AWS_PROFILE"));

    let ctx = fake_auth_context(
        &[
            ("AWS_ACCESS_KEY_ID", "access-key"),
            ("AWS_SECRET_ACCESS_KEY", "secret-key"),
        ],
        &[],
    );
    let resolved = auth
        .resolve(ApiKeyAuthInput {
            ctx: ctx.as_ref(),
            credential: Some(&empty_credential),
            signal: &signal,
        })
        .await
        .expect("resolve")
        .expect("configured");
    assert_eq!(resolved.auth, ModelAuth::default());
    assert_eq!(resolved.source.as_deref(), Some("AWS access keys"));
}

struct TestInteraction {
    signal: AbortScope,
    answers: Mutex<Vec<String>>,
    events: Mutex<Vec<AuthEvent>>,
}

impl TestInteraction {
    fn new(answers: &[&str]) -> Self {
        Self {
            signal: AbortScope::new(),
            answers: Mutex::new(answers.iter().map(|answer| (*answer).to_string()).collect()),
            events: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl AuthInteraction for TestInteraction {
    fn signal(&self) -> &AbortScope {
        &self.signal
    }
    async fn prompt(&self, _request: AuthPromptRequest) -> Result<String, AuthError> {
        Ok(self.answers.lock().unwrap().remove(0))
    }
    fn notify(&self, event: &AuthEvent) {
        self.events.lock().unwrap().push(event.clone());
    }
}

fn stub_signal() -> AbortScope {
    AbortScope::new()
}

#[tokio::test]
async fn runs_provider_owned_bedrock_bearer_token_and_aws_profile_login_flows() {
    let auth = amazon_bedrock_provider()
        .auth()
        .api_key
        .clone()
        .expect("bedrock api key auth");

    let bearer = TestInteraction::new(&["bearer-token", "bedrock-token"]);
    let credential = auth.login(&bearer).await.expect("bearer login");
    assert_eq!(credential.key.as_deref(), Some("bedrock-token"));
    assert!(credential.env.is_none());

    let profile = TestInteraction::new(&["aws-profile", "work"]);
    let credential = auth.login(&profile).await.expect("profile login");
    assert!(credential.key.is_none());
    assert_eq!(
        credential.env,
        Some(
            [("AWS_PROFILE".to_string(), "work".to_string())]
                .into_iter()
                .collect()
        )
    );
    {
        let events = profile.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            AuthEvent::Info { links, .. } => {
                let links = links.as_ref().expect("links");
                assert_eq!(
                    links[0].label.as_deref(),
                    Some("AWS credential provider chain")
                );
            }
            other => panic!("expected info event, got {other:?}"),
        }
    }

    let ctx = fake_auth_context(&[], &[]);
    let signal = stub_signal();
    let resolved = auth
        .resolve(ApiKeyAuthInput {
            ctx: ctx.as_ref(),
            credential: Some(&ApiKeyCredential {
                env: Some(
                    [("AWS_PROFILE".to_string(), "work".to_string())]
                        .into_iter()
                        .collect(),
                ),
                ..Default::default()
            }),
            signal: &signal,
        })
        .await
        .expect("resolve")
        .expect("configured");
    assert_eq!(resolved.auth, ModelAuth::default());
    assert_eq!(
        resolved.env,
        Some(
            [("AWS_PROFILE".to_string(), "work".to_string())]
                .into_iter()
                .collect()
        )
    );
}

#[tokio::test]
async fn reports_bedrock_as_configured_from_ambient_aws_credentials_without_an_api_key() {
    let models = create_models(CreateModelsOptions {
        auth_context: Some(fake_auth_context(&[("AWS_PROFILE", "dev")], &[])),
        ..Default::default()
    });
    models.set_provider(amazon_bedrock_provider());
    let model = models.get_models(Some("amazon-bedrock"))[0].clone();

    let result = models
        .get_auth(
            GetAuthTarget::Provider(model.provider.as_str()),
            Default::default(),
        )
        .await
        .expect("auth")
        .expect("configured");
    assert_eq!(result.auth, ModelAuth::default());
    assert_eq!(result.source.as_deref(), Some("AWS_PROFILE"));

    let unconfigured = create_models(CreateModelsOptions {
        auth_context: Some(fake_auth_context(&[], &[])),
        ..Default::default()
    });
    unconfigured.set_provider(amazon_bedrock_provider());
    assert!(
        unconfigured
            .get_auth(
                GetAuthTarget::Provider(model.provider.as_str()),
                Default::default()
            )
            .await
            .expect("auth")
            .is_none()
    );
}

#[tokio::test]
async fn requires_cloudflare_workers_ai_account_config_and_returns_scoped_env() {
    let missing_account = create_models(CreateModelsOptions {
        auth_context: Some(fake_auth_context(&[("CLOUDFLARE_API_KEY", "cf-key")], &[])),
        ..Default::default()
    });
    missing_account.set_provider(cloudflare_workers_ai_provider());
    let model = missing_account.get_models(Some("cloudflare-workers-ai"))[0].clone();
    assert!(
        missing_account
            .get_auth(
                GetAuthTarget::Provider(model.provider.as_str()),
                Default::default()
            )
            .await
            .expect("auth")
            .is_none()
    );

    let configured = create_models(CreateModelsOptions {
        auth_context: Some(fake_auth_context(
            &[
                ("CLOUDFLARE_API_KEY", "cf-key"),
                ("CLOUDFLARE_ACCOUNT_ID", "account-id"),
            ],
            &[],
        )),
        ..Default::default()
    });
    configured.set_provider(cloudflare_workers_ai_provider());
    let result = configured
        .get_auth(
            GetAuthTarget::Provider(model.provider.as_str()),
            Default::default(),
        )
        .await
        .expect("auth")
        .expect("configured");
    assert_eq!(result.auth.api_key.as_deref(), Some("cf-key"));
    assert_eq!(
        result.env,
        Some(
            [(
                "CLOUDFLARE_ACCOUNT_ID".to_string(),
                "account-id".to_string()
            )]
            .into_iter()
            .collect()
        )
    );
}

#[tokio::test]
async fn requires_cloudflare_ai_gateway_account_and_gateway_config_and_returns_scoped_env_headers()
{
    let missing_gateway = create_models(CreateModelsOptions {
        auth_context: Some(fake_auth_context(
            &[
                ("CLOUDFLARE_API_KEY", "cf-key"),
                ("CLOUDFLARE_ACCOUNT_ID", "account-id"),
            ],
            &[],
        )),
        ..Default::default()
    });
    missing_gateway.set_provider(cloudflare_ai_gateway_provider());
    let model = missing_gateway.get_models(Some("cloudflare-ai-gateway"))[0].clone();
    assert!(
        missing_gateway
            .get_auth(
                GetAuthTarget::Provider(model.provider.as_str()),
                Default::default()
            )
            .await
            .expect("auth")
            .is_none()
    );

    let configured = create_models(CreateModelsOptions {
        auth_context: Some(fake_auth_context(
            &[
                ("CLOUDFLARE_API_KEY", "cf-key"),
                ("CLOUDFLARE_ACCOUNT_ID", "account-id"),
                ("CLOUDFLARE_GATEWAY_ID", "gateway-id"),
            ],
            &[],
        )),
        ..Default::default()
    });
    configured.set_provider(cloudflare_ai_gateway_provider());
    let result = configured
        .get_auth(
            GetAuthTarget::Provider(model.provider.as_str()),
            Default::default(),
        )
        .await
        .expect("auth")
        .expect("configured");
    assert_eq!(
        result.auth.headers,
        Some(
            [
                (
                    "cf-aig-authorization".to_string(),
                    Some("Bearer cf-key".to_string())
                ),
                ("Authorization".to_string(), None),
                ("x-api-key".to_string(), None),
            ]
            .into_iter()
            .collect()
        )
    );
    assert_eq!(
        result.env,
        Some(
            [
                (
                    "CLOUDFLARE_ACCOUNT_ID".to_string(),
                    "account-id".to_string()
                ),
                (
                    "CLOUDFLARE_GATEWAY_ID".to_string(),
                    "gateway-id".to_string()
                ),
            ]
            .into_iter()
            .collect()
        )
    );
}

#[tokio::test]
async fn runs_provider_owned_vertex_api_key_and_adc_login_flows() {
    let auth = google_vertex_provider()
        .auth()
        .api_key
        .clone()
        .expect("vertex api key auth");

    let key = TestInteraction::new(&["api-key", "vertex-key"]);
    let credential = auth.login(&key).await.expect("api key login");
    assert_eq!(credential.key.as_deref(), Some("vertex-key"));

    let adc = TestInteraction::new(&["adc", "project-id", "us-central1"]);
    let credential = auth.login(&adc).await.expect("adc login");
    assert!(credential.key.is_none());
    assert_eq!(
        credential.env,
        Some(
            [
                ("GOOGLE_CLOUD_PROJECT".to_string(), "project-id".to_string()),
                (
                    "GOOGLE_CLOUD_LOCATION".to_string(),
                    "us-central1".to_string()
                ),
            ]
            .into_iter()
            .collect()
        )
    );
    {
        let events = adc.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            AuthEvent::Info { links, .. } => {
                let links = links.as_ref().expect("links");
                assert_eq!(
                    links[0].label.as_deref(),
                    Some("Application Default Credentials")
                );
            }
            other => panic!("expected info event, got {other:?}"),
        }
    }

    let adc_path = "~/.config/gcloud/application_default_credentials.json";
    let ctx = fake_auth_context(&[], &[adc_path]);
    let signal = stub_signal();
    let stored = ApiKeyCredential {
        env: Some(
            [
                ("GOOGLE_CLOUD_PROJECT".to_string(), "project-id".to_string()),
                (
                    "GOOGLE_CLOUD_LOCATION".to_string(),
                    "us-central1".to_string(),
                ),
            ]
            .into_iter()
            .collect(),
        ),
        ..Default::default()
    };
    let resolved = auth
        .resolve(ApiKeyAuthInput {
            ctx: ctx.as_ref(),
            credential: Some(&stored),
            signal: &signal,
        })
        .await
        .expect("resolve")
        .expect("configured");
    assert_eq!(resolved.auth, ModelAuth::default());
    assert_eq!(resolved.env, stored.env);
}

#[tokio::test]
async fn resolves_vertex_via_adc_file_plus_project_and_location() {
    let adc = "~/.config/gcloud/application_default_credentials.json";
    let configured = create_models(CreateModelsOptions {
        auth_context: Some(fake_auth_context(
            &[
                ("GOOGLE_CLOUD_PROJECT", "proj"),
                ("GOOGLE_CLOUD_LOCATION", "us-central1"),
            ],
            &[adc],
        )),
        ..Default::default()
    });
    configured.set_provider(google_vertex_provider());
    let model = configured.get_models(Some("google-vertex"))[0].clone();

    let result = configured
        .get_auth(
            GetAuthTarget::Provider(model.provider.as_str()),
            Default::default(),
        )
        .await
        .expect("auth")
        .expect("configured");
    assert_eq!(result.auth, ModelAuth::default());
    assert!(
        result
            .source
            .as_deref()
            .unwrap_or_default()
            .contains("application default")
    );

    // ADC without project/location is not configured
    let partial = create_models(CreateModelsOptions {
        auth_context: Some(fake_auth_context(
            &[("GOOGLE_CLOUD_PROJECT", "proj")],
            &[adc],
        )),
        ..Default::default()
    });
    partial.set_provider(google_vertex_provider());
    assert!(
        partial
            .get_auth(
                GetAuthTarget::Provider(model.provider.as_str()),
                Default::default()
            )
            .await
            .expect("auth")
            .is_none()
    );

    // explicit key wins over ADC
    let keyed = create_models(CreateModelsOptions {
        auth_context: Some(fake_auth_context(
            &[("GOOGLE_CLOUD_API_KEY", "vertex-key")],
            &[],
        )),
        ..Default::default()
    });
    keyed.set_provider(google_vertex_provider());
    assert_eq!(
        keyed
            .get_auth(
                GetAuthTarget::Provider(model.provider.as_str()),
                Default::default()
            )
            .await
            .expect("auth")
            .expect("configured")
            .auth
            .api_key
            .as_deref(),
        Some("vertex-key")
    );
}

#[tokio::test]
async fn env_api_key_auth_prefers_the_stored_credential_key_and_falls_back_in_order() {
    let auth = env_api_key_auth("Test key", &["FIRST_KEY", "SECOND_KEY"]);

    let ctx = fake_auth_context(&[("FIRST_KEY", "env")], &[]);
    let signal = stub_signal();
    let stored = auth
        .resolve(ApiKeyAuthInput {
            ctx: ctx.as_ref(),
            credential: Some(&ApiKeyCredential {
                key: Some("stored".into()),
                ..Default::default()
            }),
            signal: &signal,
        })
        .await
        .expect("resolve")
        .expect("configured");
    assert_eq!(stored.auth.api_key.as_deref(), Some("stored"));
    assert_eq!(stored.source.as_deref(), Some("stored credential"));

    let second_ctx = fake_auth_context(&[("SECOND_KEY", "second")], &[]);
    let second = auth
        .resolve(ApiKeyAuthInput {
            ctx: second_ctx.as_ref(),
            credential: None,
            signal: &signal,
        })
        .await
        .expect("resolve")
        .expect("configured");
    assert_eq!(second.auth.api_key.as_deref(), Some("second"));
    assert_eq!(second.source.as_deref(), Some("SECOND_KEY"));

    let empty_ctx = fake_auth_context(&[], &[]);
    assert!(
        auth.resolve(ApiKeyAuthInput {
            ctx: empty_ctx.as_ref(),
            credential: None,
            signal: &signal,
        })
        .await
        .expect("resolve")
        .is_none()
    );
}

#[tokio::test]
async fn env_api_key_auth_login_prompts_for_a_secret_and_returns_an_api_key_credential() {
    let auth = env_api_key_auth("Test key", &["TEST_KEY"]);
    let interaction = TestInteraction::new(&["entered-key"]);
    let credential = auth.login(&interaction).await.expect("login");
    assert_eq!(credential.key.as_deref(), Some("entered-key"));
    assert!(credential.env.is_none());
}

// ---------------------------------------------------------------------------
// createProvider
// ---------------------------------------------------------------------------

type Calls = Arc<Mutex<Vec<String>>>;

struct RecordingStreams {
    label: &'static str,
    calls: Calls,
}

impl RecordingStreams {
    fn respond(&self, model: &Model) -> AssistantMessageEventStream {
        self.calls
            .lock()
            .unwrap()
            .push(format!("{}:{}", self.label, model.id));
        let stream = AssistantMessageEventStream::new();
        let message = done_message(model, "ok");
        let _ = stream.push(AssistantMessageEvent::Start {
            partial: Box::new(message.clone()),
        });
        let _ = stream.push(AssistantMessageEvent::Done {
            reason: DoneReason::Stop,
            message: Box::new(message.clone()),
        });
        stream.end(message);
        stream
    }
}

#[async_trait]
impl ProviderStreams for RecordingStreams {
    fn stream(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: StreamOptions,
    ) -> AssistantMessageEventStream {
        self.respond(model)
    }
    fn stream_simple(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        self.respond(model)
    }
}

struct LazyStreams {
    calls: Calls,
    context: TranscriptContext,
}

#[async_trait]
impl ProviderStreams for LazyStreams {
    fn stream(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: StreamOptions,
    ) -> AssistantMessageEventStream {
        self.respond(model)
    }
    fn stream_simple(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        self.respond(model)
    }
    fn has_fetch_deferred(&self) -> bool {
        true
    }
    fn fetch_deferred(
        &self,
        model: &Model,
        _handle: &DeferredHandle,
        _options: DeferredFetchOptions,
    ) -> Option<AssistantMessageEventStream> {
        Some(self.stream_simple(model, &self.context, SimpleStreamOptions::default()))
    }
}

impl LazyStreams {
    fn respond(&self, model: &Model) -> AssistantMessageEventStream {
        self.calls
            .lock()
            .unwrap()
            .push(format!("deferred:{}", model.id));
        let stream = AssistantMessageEventStream::new();
        let message = done_message(model, "ok");
        let _ = stream.push(AssistantMessageEvent::Start {
            partial: Box::new(message.clone()),
        });
        let _ = stream.push(AssistantMessageEvent::Done {
            reason: DoneReason::Stop,
            message: Box::new(message.clone()),
        });
        stream.end(message);
        stream
    }
}

fn model_for(provider: &str, api: &str, id: &str) -> Model {
    let mut model = test_model(provider, id);
    model.api = api.into();
    model
}

fn mixed_model(api: &str, id: &str) -> Model {
    model_for("mixed", api, id)
}

#[tokio::test]
async fn lazily_exposes_only_declared_deferred_capabilities() {
    let loads = Arc::new(AtomicUsize::new(0));
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let context = user_context();
    let api: Arc<dyn ProviderStreams> = lazy_api(
        {
            let loads = loads.clone();
            let calls = calls.clone();
            let context = context.clone();
            Arc::new(move || {
                let loads = loads.clone();
                let calls = calls.clone();
                let context = context.clone();
                Box::pin(async move {
                    loads.fetch_add(1, Ordering::SeqCst);
                    Ok(Arc::new(LazyStreams { calls, context }) as Arc<dyn ProviderStreams>)
                }) as kiana_ai::api::lazy::LazyApiLoadFuture
            }) as LazyApiLoad
        },
        Some(LazyApiCapabilities {
            fetch_deferred: true,
            cancel_deferred: false,
        }),
    );
    let model = mixed_model("api-a", "model-a");
    let handle = DeferredHandle {
        provider: model.provider.clone(),
        model_id: model.id.clone(),
        api: model.api.clone(),
        id: "response-1".into(),
        expires_at: None,
        poll_after_ms: None,
        data: None,
    };

    assert_eq!(loads.load(Ordering::SeqCst), 0);
    assert!(!api.has_cancel_deferred());
    let result = api
        .fetch_deferred(
            &model,
            &handle,
            DeferredFetchOptions {
                base: StreamOptions::default(),
                wait: None,
            },
        )
        .expect("fetch deferred exposed")
        .result()
        .await
        .expect("final message");
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    assert_eq!(loads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dispatches_on_model_api_for_mixed_api_providers() {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let provider = create_provider(CreateProviderOptions {
        id: "mixed".into(),
        auth: ambient_auth(),
        models: vec![
            kiana_ai::types::AnyModel::Chat(mixed_model("api-a", "model-a")),
            kiana_ai::types::AnyModel::Chat(mixed_model("api-b", "model-b")),
        ],
        api: Some(ProviderApiImpl::ByApi(IndexMap::from([
            (
                "api-a".to_string(),
                Arc::new(RecordingStreams {
                    label: "a",
                    calls: calls.clone(),
                }) as Arc<dyn ProviderStreams>,
            ),
            (
                "api-b".to_string(),
                Arc::new(RecordingStreams {
                    label: "b",
                    calls: calls.clone(),
                }) as Arc<dyn ProviderStreams>,
            ),
        ]))),
        ..Default::default()
    })
    .expect("provider");
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(provider);

    models
        .complete_simple(
            &mixed_model("api-a", "model-a"),
            &Context::default(),
            ModelsSimpleStreamOptions::default(),
        )
        .await;
    models
        .complete_simple(
            &mixed_model("api-b", "model-b"),
            &Context::default(),
            ModelsSimpleStreamOptions::default(),
        )
        .await;
    assert_eq!(*calls.lock().unwrap(), vec!["a:model-a", "b:model-b"]);
}

struct CapturingStreams {
    env: Arc<Mutex<Option<IndexMap<String, String>>>>,
    api_key: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl ProviderStreams for CapturingStreams {
    fn stream(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        *self.env.lock().unwrap() = options.env.clone();
        *self.api_key.lock().unwrap() = options.api_key.clone();
        let stream = AssistantMessageEventStream::new();
        let message = done_message(model, "ok");
        stream.end(message);
        stream
    }
    fn stream_simple(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        *self.env.lock().unwrap() = options.base.env.clone();
        *self.api_key.lock().unwrap() = options.base.api_key.clone();
        let stream = AssistantMessageEventStream::new();
        let message = done_message(model, "ok");
        stream.end(message);
        stream
    }
}

#[tokio::test]
async fn merges_provider_resolved_env_into_stream_options() {
    let env = Arc::new(Mutex::new(None));
    let api_key = Arc::new(Mutex::new(None));
    let mut env_model = test_model("env-provider", "model-a");
    env_model.api = "api-a".into();
    let provider = create_provider(CreateProviderOptions {
        id: "env-provider".into(),
        auth: ProviderAuth {
            api_key: Some(Arc::new(ProviderEnvAuth)),
            oauth: None,
        },
        models: vec![kiana_ai::types::AnyModel::Chat(env_model.clone())],
        api: Some(ProviderApiImpl::Single(Arc::new(CapturingStreams {
            env: env.clone(),
            api_key: api_key.clone(),
        }))),
        ..Default::default()
    })
    .expect("provider");
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(provider);

    models
        .complete_simple(
            &env_model,
            &Context::default(),
            ModelsSimpleStreamOptions::from(SimpleStreamOptions {
                base: StreamOptions {
                    api_key: Some("request-key".into()),
                    env: Some(
                        [
                            ("REQUEST_ONLY".to_string(), "request".to_string()),
                            ("SHARED".to_string(), "request".to_string()),
                        ]
                        .into_iter()
                        .collect(),
                    ),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )
        .await;

    assert_eq!(api_key.lock().unwrap().as_deref(), Some("request-key"));
    assert_eq!(
        *env.lock().unwrap(),
        Some(
            [
                ("PROVIDER_ONLY".to_string(), "provider".to_string()),
                ("REQUEST_ONLY".to_string(), "request".to_string()),
                ("SHARED".to_string(), "request".to_string()),
            ]
            .into_iter()
            .collect::<IndexMap<_, _>>()
        )
    );
}

struct ProviderEnvAuth;

#[async_trait]
impl ApiKeyAuth for ProviderEnvAuth {
    fn name(&self) -> &str {
        "Test"
    }
    async fn resolve(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some("provider-key".into()),
                headers: Some(
                    [
                        (
                            "Authorization".to_string(),
                            Some("Bearer provider".to_string()),
                        ),
                        ("X-Shared".to_string(), Some("provider".to_string())),
                    ]
                    .into_iter()
                    .collect(),
                ),
                base_url: Some("https://resolved.test/v1".into()),
            },
            env: Some(
                [
                    ("PROVIDER_ONLY".to_string(), "provider".to_string()),
                    ("SHARED".to_string(), "provider".to_string()),
                ]
                .into_iter()
                .collect(),
            ),
            source: None,
        }))
    }
}

#[derive(Default)]
struct CapturedDeferred {
    model_base_url: Option<String>,
    wait: Option<u64>,
    timeout_ms: Option<u64>,
    api_key: Option<String>,
    headers: Option<IndexMap<String, Option<String>>>,
    env: Option<IndexMap<String, String>>,
}

#[derive(Default)]
struct DeferredStreams {
    fetched: Arc<Mutex<CapturedDeferred>>,
    cancelled: Arc<Mutex<CapturedDeferred>>,
}

#[async_trait]
impl ProviderStreams for DeferredStreams {
    fn stream(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: StreamOptions,
    ) -> AssistantMessageEventStream {
        let stream = AssistantMessageEventStream::new();
        let message = done_message(model, "ok");
        stream.end(message);
        stream
    }
    fn stream_simple(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        let stream = AssistantMessageEventStream::new();
        let message = done_message(model, "ok");
        stream.end(message);
        stream
    }
    fn has_fetch_deferred(&self) -> bool {
        true
    }
    fn fetch_deferred(
        &self,
        model: &Model,
        _handle: &DeferredHandle,
        options: DeferredFetchOptions,
    ) -> Option<AssistantMessageEventStream> {
        {
            let mut captured = self.fetched.lock().unwrap();
            captured.model_base_url = Some(model.base_url.clone());
            captured.wait = options.wait;
            captured.timeout_ms = options.base.timeout_ms;
            captured.api_key = options.base.api_key.clone();
            captured.headers = options.base.headers.clone();
            captured.env = options.base.env.clone();
        }
        Some(self.stream_simple(model, &user_context(), SimpleStreamOptions::default()))
    }
    fn has_cancel_deferred(&self) -> bool {
        true
    }
    async fn cancel_deferred(
        &self,
        _model: &Model,
        _handle: &DeferredHandle,
        options: kiana_ai::types::DeferredCancelOptions,
    ) -> Option<Result<(), kiana_ai::error::AiError>> {
        let mut captured = self.cancelled.lock().unwrap();
        captured.timeout_ms = options.timeout_ms;
        captured.api_key = options.api_key.clone();
        captured.headers = options.headers.clone();
        captured.env = options.env.clone();
        Some(Ok(()))
    }
}

#[tokio::test]
async fn applies_resolved_request_options_to_deferred_fetch_and_cancellation() {
    let streams = Arc::new(DeferredStreams::default());
    let mut deferred_model = test_model("deferred-provider", "model-a");
    deferred_model.api = "api-a".into();
    let provider = create_provider(CreateProviderOptions {
        id: "deferred-provider".into(),
        auth: ProviderAuth {
            api_key: Some(Arc::new(ProviderEnvAuth)),
            oauth: None,
        },
        models: vec![kiana_ai::types::AnyModel::Chat(deferred_model.clone())],
        api: Some(ProviderApiImpl::Single(streams.clone())),
        ..Default::default()
    })
    .expect("provider");
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(provider);
    let handle = DeferredHandle {
        provider: deferred_model.provider.clone(),
        model_id: deferred_model.id.clone(),
        api: deferred_model.api.clone(),
        id: "response-1".into(),
        expires_at: None,
        poll_after_ms: None,
        data: None,
    };

    let transform: TransformHeadersFn = Arc::new(|mut headers| {
        headers.insert("X-Transformed".into(), Some("yes".into()));
        Box::pin(async move { headers })
    });
    models
        .fetch_deferred(
            &deferred_model,
            &handle,
            ModelsDeferredFetchOptions {
                base: DeferredFetchOptions {
                    base: StreamOptions {
                        timeout_ms: Some(100),
                        api_key: Some("request-key".into()),
                        headers: Some(
                            [
                                ("X-Request".to_string(), Some("request".to_string())),
                                ("x-shared".to_string(), Some("request".to_string())),
                            ]
                            .into_iter()
                            .collect(),
                        ),
                        env: Some(
                            [
                                ("REQUEST_ONLY".to_string(), "request".to_string()),
                                ("SHARED".to_string(), "request".to_string()),
                            ]
                            .into_iter()
                            .collect(),
                        ),
                        ..Default::default()
                    },
                    wait: Some(50),
                },
                transforms: ModelsRequestTransforms {
                    transform_headers: Some(transform),
                },
                ..Default::default()
            },
        )
        .await;
    let cancel_transform: TransformHeadersFn = Arc::new(|mut headers| {
        headers.insert("X-Cancel".into(), Some("yes".into()));
        Box::pin(async move { headers })
    });
    models
        .cancel_deferred(
            &deferred_model,
            &handle,
            ModelsDeferredCancelOptions {
                base: StreamOptions {
                    timeout_ms: Some(200),
                    ..Default::default()
                },
                transforms: ModelsRequestTransforms {
                    transform_headers: Some(cancel_transform),
                },
                ..Default::default()
            },
        )
        .await
        .expect("cancel");

    let fetched = streams.fetched.lock().unwrap();
    assert_eq!(
        fetched.model_base_url.as_deref(),
        Some("https://resolved.test/v1")
    );
    assert_eq!(fetched.wait, Some(50));
    assert_eq!(fetched.timeout_ms, Some(100));
    assert_eq!(fetched.api_key.as_deref(), Some("request-key"));
    let headers = fetched.headers.clone().expect("headers");
    assert_eq!(
        headers.get("Authorization").cloned().flatten().as_deref(),
        Some("Bearer provider")
    );
    assert_eq!(
        headers.get("X-Request").cloned().flatten().as_deref(),
        Some("request")
    );
    assert_eq!(
        headers.get("x-shared").cloned().flatten().as_deref(),
        Some("request")
    );
    assert_eq!(
        headers.get("X-Transformed").cloned().flatten().as_deref(),
        Some("yes")
    );
    assert_eq!(
        fetched.env,
        Some(
            [
                ("PROVIDER_ONLY".to_string(), "provider".to_string()),
                ("REQUEST_ONLY".to_string(), "request".to_string()),
                ("SHARED".to_string(), "request".to_string()),
            ]
            .into_iter()
            .collect()
        )
    );
    drop(fetched);

    let cancelled = streams.cancelled.lock().unwrap();
    assert_eq!(cancelled.timeout_ms, Some(200));
    assert_eq!(cancelled.api_key.as_deref(), Some("provider-key"));
    let headers = cancelled.headers.clone().expect("headers");
    assert_eq!(
        headers.get("Authorization").cloned().flatten().as_deref(),
        Some("Bearer provider")
    );
    assert_eq!(
        headers.get("X-Shared").cloned().flatten().as_deref(),
        Some("provider")
    );
    assert_eq!(
        headers.get("X-Cancel").cloned().flatten().as_deref(),
        Some("yes")
    );
    assert_eq!(
        cancelled.env,
        Some(
            [
                ("PROVIDER_ONLY".to_string(), "provider".to_string()),
                ("SHARED".to_string(), "provider".to_string()),
            ]
            .into_iter()
            .collect()
        )
    );
}

#[tokio::test]
async fn produces_a_stream_error_for_a_model_whose_api_has_no_implementation() {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let provider = create_provider(CreateProviderOptions {
        id: "mixed".into(),
        auth: ambient_auth(),
        models: vec![kiana_ai::types::AnyModel::Chat(mixed_model(
            "api-a", "model-a",
        ))],
        api: Some(ProviderApiImpl::ByApi(IndexMap::from([(
            "api-a".to_string(),
            Arc::new(RecordingStreams { label: "a", calls }) as Arc<dyn ProviderStreams>,
        )]))),
        ..Default::default()
    })
    .expect("provider");

    let result = provider
        .stream_simple(
            &mixed_model("api-ghost", "model-x"),
            &user_context(),
            SimpleStreamOptions::default(),
        )
        .result()
        .await
        .expect("final message");
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("no API implementation")
    );
}

#[tokio::test]
async fn lets_a_newer_dynamic_refresh_bypass_and_supersede_older_network_work() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(Mutex::new(None::<tokio::sync::oneshot::Sender<()>>));
    let blocked = Arc::new(Mutex::new(None::<tokio::sync::oneshot::Receiver<()>>));
    let (blocked_tx, blocked_rx) = tokio::sync::oneshot::channel::<()>();
    *blocked.lock().unwrap() = Some(blocked_rx);
    let blocked_tx = Arc::new(Mutex::new(Some(blocked_tx)));

    let provider = create_provider(CreateProviderOptions {
        id: "dynamic".into(),
        auth: ambient_auth(),
        models: Vec::new(),
        fetch_models: Some({
            let fetches = fetches.clone();
            let started = started.clone();
            let blocked = blocked.clone();
            Arc::new(
                move |_context| -> futures::future::BoxFuture<
                    'static,
                    Result<Vec<kiana_ai::types::AnyModel>, kiana_ai::error::AiError>,
                > {
                    let fetches = fetches.clone();
                    let started = started.clone();
                    let blocked = blocked.clone();
                    Box::pin(async move {
                        let current = fetches.fetch_add(1, Ordering::SeqCst) + 1;
                        if current == 1 {
                            if let Some(sender) = started.lock().unwrap().take() {
                                let _ = sender.send(());
                            }
                            let receiver = blocked.lock().unwrap().take();
                            if let Some(receiver) = receiver {
                                let _ = receiver.await;
                            }
                        }
                        Ok(vec![kiana_ai::types::AnyModel::Chat(model_for(
                            "dynamic",
                            "api-a",
                            &format!("listed-{current}"),
                        ))])
                    })
                },
            ) as kiana_ai::models::FetchModelsFn
        }),
        api: Some(ProviderApiImpl::Single(Arc::new(RecordingStreams {
            label: "a",
            calls: Arc::new(Mutex::new(Vec::new())),
        }))),
        ..Default::default()
    })
    .expect("provider");

    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    *started.lock().unwrap() = Some(started_tx);

    let store = Arc::new(InMemoryModelsStore::new());
    let models = create_models(CreateModelsOptions {
        models_store: Some(store.clone()),
        ..Default::default()
    });
    models.set_provider(provider.clone());
    assert!(provider.get_models().is_empty());

    let models_for_first = models.clone();
    let first = async move {
        models_for_first
            .refresh(ModelsRefreshOptions {
                providers: Some(vec!["dynamic".into()]),
                ..Default::default()
            })
            .await
    };
    tokio::pin!(first);
    tokio::select! {
        result = &mut first => panic!("first refresh finished before its fetch blocked: {result:?}"),
        _ = started_rx => {}
    }
    let second = models
        .refresh(ModelsRefreshOptions {
            providers: Some(vec!["dynamic".into()]),
            ..Default::default()
        })
        .await
        .expect("second refresh");
    assert!(!second.aborted);
    let first = first.await.expect("first refresh");
    assert!(!first.aborted);
    assert_eq!(fetches.load(Ordering::SeqCst), 2);
    assert_eq!(
        provider
            .get_models()
            .into_iter()
            .map(|model| model.id)
            .collect::<Vec<_>>(),
        vec!["listed-2"]
    );
    assert_eq!(
        store
            .read("dynamic", Default::default())
            .await
            .expect("read")
            .expect("stored")
            .models
            .into_iter()
            .map(|model| model.model().id.clone())
            .collect::<Vec<_>>(),
        vec!["listed-2"]
    );

    if let Some(sender) = blocked_tx.lock().unwrap().take() {
        let _ = sender.send(());
    }
    tokio::task::yield_now().await;
    assert_eq!(
        provider
            .get_models()
            .into_iter()
            .map(|model| model.id)
            .collect::<Vec<_>>(),
        vec!["listed-2"]
    );
    assert_eq!(
        store
            .read("dynamic", Default::default())
            .await
            .expect("read")
            .expect("stored")
            .models
            .into_iter()
            .map(|model| model.model().id.clone())
            .collect::<Vec<_>>(),
        vec!["listed-2"]
    );
}

// ---------------------------------------------------------------------------
// fauxProvider
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streams_queued_faux_responses_through_a_models_collection() {
    let faux = faux_provider();
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(faux.provider.clone());
    faux.set_responses(vec![
        faux_assistant_message("hello from faux", FauxAssistantMessageOptions::default()).into(),
    ]);

    let model = models.get_models(Some(faux.provider.id()))[0].clone();
    let result = models
        .complete_simple(
            &model,
            &plain_context(),
            ModelsSimpleStreamOptions::default(),
        )
        .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    match &result.content[0] {
        AssistantContentBlock::Text(text) => assert_eq!(text.text, "hello from faux"),
        other => panic!("expected text, got {other:?}"),
    }
    assert_eq!(faux.state().call_count(), 1);
}

#[tokio::test]
async fn submits_polls_and_redeems_deferred_responses() {
    let faux = faux_provider_with(RegisterFauxProviderOptions {
        deferred: Some(FauxDeferredOptions {
            pending_fetches: Some(1.0),
            poll_after_ms: Some(25.0),
        }),
        ..Default::default()
    });
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(faux.provider.clone());
    faux.set_responses(vec![
        faux_assistant_message("ready", FauxAssistantMessageOptions::default()).into(),
    ]);
    let model = faux.get_model(None).expect("faux model").clone();

    let submission = models.stream_simple(
        &model,
        &plain_context(),
        ModelsSimpleStreamOptions::from(SimpleStreamOptions {
            deferred: Some(DeferredRequest::Window {
                window: Some(DeferredWindow::H1),
            }),
            ..Default::default()
        }),
    );
    let mut event_types = Vec::new();
    while let Some(event) = submission.recv().await {
        let kind = event.kind().to_string();
        let terminal = matches!(
            event,
            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
        );
        event_types.push(kind);
        if terminal {
            break;
        }
    }
    let deferred = submission.result().await.expect("deferred message");
    assert_eq!(event_types, vec!["start", "done"]);
    assert_eq!(deferred.stop_reason, kiana_ai::types::StopReason::Deferred);
    assert!(deferred.content.is_empty());
    let handle = deferred.deferred.clone().expect("deferred handle");
    assert_eq!(handle.provider, model.provider);
    assert_eq!(handle.model_id, model.id);
    assert_eq!(handle.api, model.api);
    assert!(!handle.id.is_empty());
    assert_eq!(handle.poll_after_ms, Some(25.0));

    let pending = models
        .fetch_deferred(&model, &handle, ModelsDeferredFetchOptions::default())
        .await;
    assert_eq!(pending.stop_reason, kiana_ai::types::StopReason::Deferred);
    assert_eq!(pending.deferred.as_ref(), Some(&handle));

    let ready = models
        .fetch_deferred(
            &model,
            &handle,
            ModelsDeferredFetchOptions {
                base: DeferredFetchOptions {
                    base: StreamOptions::default(),
                    wait: Some(0),
                },
                ..Default::default()
            },
        )
        .await;
    assert_eq!(ready.stop_reason, kiana_ai::types::StopReason::Stop);
    match &ready.content[0] {
        AssistantContentBlock::Text(text) => assert_eq!(text.text, "ready"),
        other => panic!("expected text, got {other:?}"),
    }
    assert!(ready.usage.total_tokens > 0);
    assert_eq!(faux.state().call_count(), 1);
    assert_eq!(faux.state().deferred_fetch_count(), 2);
}

#[tokio::test]
async fn records_cancellation_and_returns_deferred_fetch_failures_in_band() {
    let faux = faux_provider();
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(faux.provider.clone());
    faux.set_responses(vec![
        FauxResponseStep::factory(|_input| async {
            Err(ThrownValue::Error(kiana_ai::types::DiagnosticErrorInfo {
                name: Some("Error".into()),
                message: "deferred failed".into(),
                stack: None,
                code: None,
            }))
        }),
        faux_assistant_message("cancelled", FauxAssistantMessageOptions::default()).into(),
    ]);
    let model = faux.get_model(None).expect("faux model").clone();

    let failed_submission = models
        .complete_simple(
            &model,
            &plain_context(),
            ModelsSimpleStreamOptions::from(SimpleStreamOptions {
                deferred: Some(DeferredRequest::Enabled(true)),
                ..Default::default()
            }),
        )
        .await;
    let failed_handle = failed_submission.deferred.clone().expect("deferred handle");
    let failed = models
        .fetch_deferred(
            &model,
            &failed_handle,
            ModelsDeferredFetchOptions::default(),
        )
        .await;
    assert_eq!(failed.stop_reason, kiana_ai::types::StopReason::Error);
    assert_eq!(failed.error_message.as_deref(), Some("deferred failed"));

    let cancelled_submission = models
        .complete_simple(
            &model,
            &plain_context(),
            ModelsSimpleStreamOptions::from(SimpleStreamOptions {
                deferred: Some(DeferredRequest::Enabled(true)),
                ..Default::default()
            }),
        )
        .await;
    let cancelled_handle = cancelled_submission
        .deferred
        .clone()
        .expect("deferred handle");
    models
        .cancel_deferred(
            &model,
            &cancelled_handle,
            ModelsDeferredCancelOptions::default(),
        )
        .await
        .expect("cancel");
    assert_eq!(
        faux.state().cancelled_deferred(),
        vec![cancelled_handle.clone()]
    );
    let cancelled = models
        .fetch_deferred(
            &model,
            &cancelled_handle,
            ModelsDeferredFetchOptions::default(),
        )
        .await;
    assert_eq!(cancelled.stop_reason, kiana_ai::types::StopReason::Error);
    assert!(
        cancelled
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("was cancelled")
    );
}
