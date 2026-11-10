//! `model-registry.test.ts` 的移植（87 个用例）。
//!
//! TS 源：`packages/coding-agent/test/model-registry.test.ts`。规范：
//! `spec/ca-config-models.md` §2.22（ModelRegistry）、§7.2（测试映射）。

#[path = "fixtures/model_runtime/mod.rs"]
mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use kiana_ai::models::get_supported_thinking_levels;
use kiana_ai::types::{
    Api, Model, ModelCost, ModelInputModality, ModelPromptCache, ModelThinkingLevel,
    PromptCacheTier, ProviderHeaders, SimpleStreamOptions, ThinkingLevelMap, TranscriptContext,
};
use kiana_config::auth::models_store::InMemoryCodingAgentModelsStore;
use kiana_config::models::composer::{
    AuthStatus, AuthStatusSource, ExtensionGetApiKeyFn, ExtensionLoginFn, ExtensionOAuthConfig,
    ExtensionRefreshTokenFn, ProviderChatModelConfig, ProviderConfigInput, ProviderModelConfig,
    ProviderModelConfigBase,
};
use kiana_config::models::registry::{ResolvedAuth, ResolvedRequestAuth};
use kiana_config::models::runtime::ModelRuntime;
use serde_json::{Value, json};
use support::*;

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

/// TS `providerConfig(baseUrl, models, api = "anthropic-messages")` 写进 models.json 的形状。
fn provider_json(base_url: &str, models: &[&str], api: &str) -> Value {
    json!({
        "baseUrl": base_url,
        "apiKey": "test-key",
        "api": api,
        "models": models
            .iter()
            .map(|id| json!({
                "id": id,
                "name": id,
                "reasoning": false,
                "input": ["text"],
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                "contextWindow": 100000,
                "maxTokens": 8000
            }))
            .collect::<Vec<_>>()
    })
}

/// TS `providerWithApiKey(apiKey)`。
fn provider_with_api_key(api_key: &str) -> Value {
    json!({
        "baseUrl": "https://example.com/v1",
        "apiKey": api_key,
        "api": "anthropic-messages",
        "models": [{
            "id": "test-model",
            "name": "Test Model",
            "reasoning": false,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 100000,
            "maxTokens": 8000
        }]
    })
}

/// `overrideConfig(baseUrl, headers?)`。
fn override_config(base_url: &str) -> Value {
    json!({ "baseUrl": base_url })
}

/// 扩展注册的 chat 模型配置。
fn chat_model_config(id: &str, name: &str) -> ProviderModelConfig {
    ProviderModelConfig::Chat(ProviderChatModelConfig {
        base: ProviderModelConfigBase {
            id: id.to_string(),
            name: name.to_string(),
            api: None,
            base_url: None,
            input: vec![ModelInputModality::Text],
            input_limits: None,
            cost: ModelCost::default(),
            headers: None,
            declared_type: None,
        },
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: 128000.0,
        max_tokens: 4096.0,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    })
}

/// `registerProvider(name, { baseUrl, apiKey, api, models })`。
fn registered_provider(
    base_url: &str,
    api: &str,
    api_key: Option<&str>,
    models: &[(&str, &str)],
) -> ProviderConfigInput {
    ProviderConfigInput {
        base_url: Some(base_url.to_string()),
        api: Some(api.to_string()),
        api_key: api_key.map(str::to_string),
        models: Some(
            models
                .iter()
                .map(|(id, name)| chat_model_config(id, name))
                .collect(),
        ),
        ..Default::default()
    }
}

/// 扩展 OAuth 配置（login/refresh/getApiKey 的测试桩）。
fn extension_oauth(name: &str, is_subscription: Option<bool>) -> ExtensionOAuthConfig {
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
    let refresh_token: Arc<ExtensionRefreshTokenFn> =
        Arc::new(|credential, _signal| Box::pin(async move { Ok(credential) }));
    let get_api_key: Arc<ExtensionGetApiKeyFn> = Arc::new(|credential| credential.access.clone());
    ExtensionOAuthConfig {
        name: name.to_string(),
        is_subscription,
        uses_callback_server: None,
        login,
        refresh_token,
        get_api_key,
        modify_models: None,
    }
}

/// `process.env[name] = value`（Edition 2024 的 unsafe 环境写入）。
fn set_env(name: &str, value: &str) {
    // SAFETY: 各测试使用互不相同的环境变量名；测试进程内没有其它线程读取这些名字。
    unsafe { std::env::set_var(name, value) };
}

/// 清理测试设置的环境变量。
fn remove_env(name: &str) {
    // SAFETY: 同 `set_env`。
    unsafe { std::env::remove_var(name) };
}

fn headers(pairs: &[(&str, Option<&str>)]) -> ProviderHeaders {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.map(str::to_string)))
        .collect()
}

// ===========================================================================
// baseUrl override (no custom models)
// ===========================================================================

#[tokio::test]
async fn base_url_override_keeps_all_builtin_models() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "anthropic": override_config("https://my-proxy.example.com/v1") }
    }));

    let (_runtime, registry) = harness.registry().await;
    let anthropic = models_for_provider(&registry, "anthropic");

    assert!(anthropic.len() > 1);
    assert!(anthropic.iter().any(|model| model.id.contains("claude")));
}

#[tokio::test]
async fn base_url_override_changes_url_on_all_builtin_models() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "anthropic": override_config("https://my-proxy.example.com/v1") }
    }));

    let (_runtime, registry) = harness.registry().await;
    let anthropic = models_for_provider(&registry, "anthropic");

    for model in anthropic {
        assert_eq!(model.base_url, "https://my-proxy.example.com/v1");
    }
}

#[tokio::test]
async fn base_url_override_resolves_headers_at_request_time() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "anthropic": {
            "baseUrl": "https://my-proxy.example.com/v1",
            "headers": { "X-Custom-Header": "custom-value" }
        }}
    }));

    let (_runtime, registry) = harness.registry().await;
    for model in models_for_provider(&registry, "anthropic") {
        let auth = registry.get_api_key_and_headers(&model).await;
        assert!(auth.is_ok());
        let ResolvedRequestAuth::Ok(resolved) = auth else {
            panic!("expected ok");
        };
        let resolved_headers = resolved.headers.expect("headers");
        assert_eq!(
            header_value(&resolved_headers, "X-Custom-Header"),
            Some(Some("custom-value"))
        );
    }
}

#[tokio::test]
async fn headers_only_override_resolves_at_request_time() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "anthropic": { "headers": { "X-Custom-Header": "custom-value" } } }
    }));

    let (_runtime, registry) = harness.registry().await;
    assert!(registry.get_error().is_none());

    for model in models_for_provider(&registry, "anthropic") {
        let auth = registry.get_api_key_and_headers(&model).await;
        let ResolvedRequestAuth::Ok(resolved) = auth else {
            panic!("expected ok");
        };
        let resolved_headers = resolved.headers.expect("headers");
        assert_eq!(
            header_value(&resolved_headers, "X-Custom-Header"),
            Some(Some("custom-value"))
        );
    }
}

#[tokio::test]
async fn unconfigured_compatibility_auth_includes_static_model_headers() {
    let harness = Harness::new();
    let (_runtime, registry) = harness.registry().await;
    let base = registry.get_all()[0].clone();
    let model = Model {
        provider: "missing-provider".to_string(),
        headers: Some(IndexMap::from([(
            "X-Static-Model".to_string(),
            "static-value".to_string(),
        )])),
        ..base
    };

    let auth = registry.get_api_key_and_headers(&model).await;

    assert_eq!(
        auth,
        ResolvedRequestAuth::Ok(ResolvedAuth {
            headers: Some(headers(&[("X-Static-Model", Some("static-value"))])),
            ..Default::default()
        })
    );
}

#[tokio::test]
async fn base_url_only_override_does_not_affect_other_providers() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "anthropic": override_config("https://my-proxy.example.com/v1") }
    }));

    let (_runtime, registry) = harness.registry().await;
    let google = models_for_provider(&registry, "google");

    assert!(!google.is_empty());
    assert_ne!(google[0].base_url, "https://my-proxy.example.com/v1");
}

#[tokio::test]
async fn can_mix_base_url_override_and_models_merge() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": {
            "anthropic": override_config("https://anthropic-proxy.example.com/v1"),
            "google": provider_json("https://google-proxy.example.com/v1", &["gemini-custom"], "google-generative-ai")
        }
    }));

    let (_runtime, registry) = harness.registry().await;

    let anthropic = models_for_provider(&registry, "anthropic");
    assert!(anthropic.len() > 1);
    assert_eq!(
        anthropic[0].base_url,
        "https://anthropic-proxy.example.com/v1"
    );

    let google = models_for_provider(&registry, "google");
    assert!(google.len() > 1);
    assert!(google.iter().any(|model| model.id == "gemini-custom"));
}

#[tokio::test]
async fn refresh_picks_up_base_url_override_changes() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "anthropic": override_config("https://first-proxy.example.com/v1") }
    }));
    let (runtime, registry) = harness.registry().await;

    assert_eq!(
        models_for_provider(&registry, "anthropic")[0].base_url,
        "https://first-proxy.example.com/v1"
    );

    harness.write_raw(json!({
        "providers": { "anthropic": override_config("https://second-proxy.example.com/v1") }
    }));
    local_refresh(&runtime).await.expect("refresh");

    assert_eq!(
        models_for_provider(&registry, "anthropic")[0].base_url,
        "https://second-proxy.example.com/v1"
    );
}

// ===========================================================================
// custom models merge behavior
// ===========================================================================

#[tokio::test]
async fn builtin_provider_custom_models_inherit_api_and_base_url() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "openrouter": { "models": [{
            "id": "fake-provider/fake-model",
            "name": "Fake model",
            "reasoning": true,
            "input": ["text"]
        }]}}
    }));

    let (_runtime, registry) = harness.registry().await;
    assert!(registry.get_error().is_none());

    let model = registry
        .find("openrouter", "fake-provider/fake-model")
        .expect("model");
    assert_eq!(model.api, "openai-completions");
    assert_eq!(model.base_url, "https://openrouter.ai/api/v1");
}

#[tokio::test]
async fn non_builtin_provider_custom_models_still_require_base_url() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "my-custom-provider": {
            "apiKey": "test-key",
            "models": [{ "id": "my-model", "api": "openai-completions", "reasoning": false, "input": ["text"] }]
        }}
    }));

    let (_runtime, registry) = harness.registry().await;
    assert!(registry.get_error().expect("error").contains("baseUrl"));
}

#[tokio::test]
async fn reports_every_provider_composition_error() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": {
            "broken-one": { "api": "openai-completions", "models": [{ "id": "one" }] },
            "broken-two": { "api": "openai-completions", "models": [{ "id": "two" }] }
        }
    }));

    let (_runtime, registry) = harness.registry().await;
    let error = registry.get_error().expect("error");

    assert!(error.contains("Provider \"broken-one\""));
    assert!(error.contains("Provider \"broken-two\""));
}

#[tokio::test]
async fn custom_provider_with_same_name_as_builtin_merges_with_builtin_models() {
    let harness = Harness::new();
    harness.write_providers(json!({
        "anthropic": provider_json("https://my-proxy.example.com/v1", &["claude-custom"], "anthropic-messages")
    }));

    let (_runtime, registry) = harness.registry().await;
    let anthropic = models_for_provider(&registry, "anthropic");

    assert!(anthropic.len() > 1);
    assert!(anthropic.iter().any(|model| model.id == "claude-custom"));
    assert!(anthropic.iter().any(|model| model.id.contains("claude")));
}

#[tokio::test]
async fn custom_model_with_same_id_replaces_builtin_model_by_id() {
    let harness = Harness::new();
    harness.write_providers(json!({
        "openrouter": provider_json(
            "https://my-proxy.example.com/v1",
            &["anthropic/claude-sonnet-4.5"],
            "openai-completions"
        )
    }));

    let (_runtime, registry) = harness.registry().await;
    let sonnet: Vec<Model> = models_for_provider(&registry, "openrouter")
        .into_iter()
        .filter(|model| model.id == "anthropic/claude-sonnet-4.5")
        .collect();

    assert_eq!(sonnet.len(), 1);
    assert_eq!(sonnet[0].base_url, "https://my-proxy.example.com/v1");
}

#[tokio::test]
async fn custom_provider_with_same_name_does_not_affect_other_builtin_providers() {
    let harness = Harness::new();
    harness.write_providers(json!({
        "anthropic": provider_json("https://my-proxy.example.com/v1", &["claude-custom"], "anthropic-messages")
    }));

    let (_runtime, registry) = harness.registry().await;

    assert!(!models_for_provider(&registry, "google").is_empty());
    assert!(!models_for_provider(&registry, "openai").is_empty());
}

#[tokio::test]
async fn provider_level_base_url_applies_to_both_builtin_and_custom_models() {
    let harness = Harness::new();
    harness.write_providers(json!({
        "anthropic": provider_json("https://merged-proxy.example.com/v1", &["claude-custom"], "anthropic-messages")
    }));

    let (_runtime, registry) = harness.registry().await;
    for model in models_for_provider(&registry, "anthropic") {
        assert_eq!(model.base_url, "https://merged-proxy.example.com/v1");
    }
}

#[tokio::test]
async fn provider_level_compat_applies_to_custom_models() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "demo": {
            "baseUrl": "https://example.com/v1",
            "apiKey": "DEMO_KEY",
            "api": "openai-completions",
            "compat": { "supportsUsageInStreaming": false, "maxTokensField": "max_tokens" },
            "models": [{
                "id": "demo-model",
                "reasoning": false,
                "input": ["text"],
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                "contextWindow": 1000,
                "maxTokens": 100
            }]
        }}
    }));

    let (_runtime, registry) = harness.registry().await;
    let model = registry.find("demo", "demo-model").expect("model");

    assert_eq!(
        compat_value(&model, "supportsUsageInStreaming"),
        Some(&json!(false))
    );
    assert_eq!(
        compat_value(&model, "maxTokensField"),
        Some(&json!("max_tokens"))
    );
}

#[tokio::test]
async fn model_level_compat_overrides_provider_level_compat() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "demo": {
            "baseUrl": "https://example.com/v1",
            "apiKey": "DEMO_KEY",
            "api": "openai-completions",
            "compat": { "supportsUsageInStreaming": false, "maxTokensField": "max_tokens" },
            "models": [{
                "id": "demo-model",
                "reasoning": false,
                "input": ["text"],
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                "contextWindow": 1000,
                "maxTokens": 100,
                "compat": { "supportsUsageInStreaming": true, "maxTokensField": "max_completion_tokens" }
            }]
        }}
    }));

    let (_runtime, registry) = harness.registry().await;
    let model = registry.find("demo", "demo-model").expect("model");

    assert_eq!(
        compat_value(&model, "supportsUsageInStreaming"),
        Some(&json!(true))
    );
    assert_eq!(
        compat_value(&model, "maxTokensField"),
        Some(&json!("max_completion_tokens"))
    );
}

#[tokio::test]
async fn provider_level_compat_applies_to_builtin_models() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "openrouter": { "compat": {
            "supportsUsageInStreaming": false,
            "supportsStrictMode": false
        }}}
    }));

    let (_runtime, registry) = harness.registry().await;
    let models = models_for_provider(&registry, "openrouter");

    assert!(!models.is_empty());
    for model in models {
        assert_eq!(
            compat_value(&model, "supportsUsageInStreaming"),
            Some(&json!(false))
        );
        assert_eq!(
            compat_value(&model, "supportsStrictMode"),
            Some(&json!(false))
        );
    }
}

#[tokio::test]
async fn model_schema_accepts_thinking_level_map_and_compat_fields() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "demo": {
            "baseUrl": "https://example.com/v1",
            "apiKey": "DEMO_KEY",
            "api": "openai-completions",
            "models": [{
                "id": "demo-model",
                "reasoning": true,
                "input": ["text"],
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                "contextWindow": 1000,
                "maxTokens": 100,
                "thinkingLevelMap": { "minimal": null, "high": "max" },
                "compat": { "supportsStrictMode": false, "cacheControlFormat": "anthropic" }
            }]
        }}
    }));

    let (_runtime, registry) = harness.registry().await;
    let model = registry.find("demo", "demo-model").expect("model");

    assert!(registry.get_error().is_none());
    let map = model.thinking_level_map.as_ref().expect("thinkingLevelMap");
    assert_eq!(map.get(&ModelThinkingLevel::Minimal), Some(&None));
    assert_eq!(
        map.get(&ModelThinkingLevel::High),
        Some(&Some("max".to_string()))
    );
    assert_eq!(
        compat_value(&model, "supportsStrictMode"),
        Some(&json!(false))
    );
    assert_eq!(
        compat_value(&model, "cacheControlFormat"),
        Some(&json!("anthropic"))
    );
}

#[tokio::test]
async fn compat_schema_accepts_chat_template_thinking_configuration() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "demo": {
            "baseUrl": "https://example.com/v1",
            "apiKey": "DEMO_KEY",
            "api": "openai-completions",
            "models": [
                {
                    "id": "kwargs-model",
                    "reasoning": true,
                    "input": ["text"],
                    "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                    "contextWindow": 1000,
                    "maxTokens": 100,
                    "compat": {
                        "thinkingFormat": "chat-template",
                        "chatTemplateKwargs": {
                            "preserve_thinking": true,
                            "thinking": { "$var": "thinking.enabled" }
                        }
                    }
                },
                {
                    "id": "args-model",
                    "reasoning": true,
                    "input": ["text"],
                    "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                    "contextWindow": 1000,
                    "maxTokens": 100,
                    "compat": {
                        "thinkingFormat": "baseten",
                        "chatTemplateArgs": { "enable_thinking": { "$var": "thinking.enabled" } }
                    }
                }
            ]
        }}
    }));

    let (_runtime, registry) = harness.registry().await;
    let kwargs = registry.find("demo", "kwargs-model").expect("model");
    let args = registry.find("demo", "args-model").expect("model");

    assert!(registry.get_error().is_none());
    assert_eq!(
        compat_value(&kwargs, "thinkingFormat"),
        Some(&json!("chat-template"))
    );
    assert_eq!(
        compat_value(&kwargs, "chatTemplateKwargs"),
        Some(&json!({
            "preserve_thinking": true,
            "thinking": { "$var": "thinking.enabled" }
        }))
    );
    assert_eq!(
        compat_value(&args, "thinkingFormat"),
        Some(&json!("baseten"))
    );
    assert_eq!(
        compat_value(&args, "chatTemplateArgs"),
        Some(&json!({ "enable_thinking": { "$var": "thinking.enabled" } }))
    );
}

#[tokio::test]
async fn compat_schema_accepts_anthropic_eager_tool_input_streaming_flag() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "demo": {
            "baseUrl": "https://example.com",
            "apiKey": "DEMO_KEY",
            "api": "anthropic-messages",
            "compat": { "supportsEagerToolInputStreaming": false },
            "models": [{
                "id": "demo-model",
                "reasoning": true,
                "input": ["text"],
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                "contextWindow": 1000,
                "maxTokens": 100
            }]
        }}
    }));

    let (_runtime, registry) = harness.registry().await;
    let model = registry.find("demo", "demo-model").expect("model");

    assert!(registry.get_error().is_none());
    assert_eq!(
        compat_value(&model, "supportsEagerToolInputStreaming"),
        Some(&json!(false))
    );
}

#[tokio::test]
async fn compat_schema_accepts_long_cache_retention_flag() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "demo": {
            "baseUrl": "https://example.com",
            "apiKey": "DEMO_KEY",
            "api": "anthropic-messages",
            "compat": { "supportsLongCacheRetention": false },
            "models": [{
                "id": "demo-model",
                "reasoning": true,
                "input": ["text"],
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                "contextWindow": 1000,
                "maxTokens": 100
            }]
        }}
    }));

    let (_runtime, registry) = harness.registry().await;
    let model = registry.find("demo", "demo-model").expect("model");

    assert!(registry.get_error().is_none());
    assert_eq!(
        compat_value(&model, "supportsLongCacheRetention"),
        Some(&json!(false))
    );
}

#[tokio::test]
async fn model_level_base_url_overrides_provider_level_base_url() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "opencode-go": {
            "baseUrl": "https://opencode.ai/zen/go/v1",
            "apiKey": "TEST_KEY",
            "models": [
                {
                    "id": "minimax-m2.5",
                    "api": "anthropic-messages",
                    "baseUrl": "https://opencode.ai/zen/go",
                    "reasoning": true,
                    "input": ["text"],
                    "cost": { "input": 0.3, "output": 1.2, "cacheRead": 0.03, "cacheWrite": 0 },
                    "contextWindow": 204800,
                    "maxTokens": 131072
                },
                {
                    "id": "glm-5",
                    "api": "openai-completions",
                    "reasoning": true,
                    "input": ["text"],
                    "cost": { "input": 1, "output": 3.2, "cacheRead": 0.2, "cacheWrite": 0 },
                    "contextWindow": 204800,
                    "maxTokens": 131072
                }
            ]
        }}
    }));

    let (_runtime, registry) = harness.registry().await;
    let m25 = registry.find("opencode-go", "minimax-m2.5").expect("m25");
    let glm5 = registry.find("opencode-go", "glm-5").expect("glm5");

    assert_eq!(m25.base_url, "https://opencode.ai/zen/go");
    assert_eq!(glm5.base_url, "https://opencode.ai/zen/go/v1");
}

#[tokio::test]
async fn model_overrides_still_apply_when_provider_also_defines_models() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "openrouter": {
            "baseUrl": "https://my-proxy.example.com/v1",
            "apiKey": "OPENROUTER_API_KEY",
            "api": "openai-completions",
            "models": [{
                "id": "custom/openrouter-model",
                "name": "Custom OpenRouter Model",
                "reasoning": false,
                "input": ["text"],
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                "contextWindow": 128000,
                "maxTokens": 16384
            }],
            "modelOverrides": {
                "anthropic/claude-sonnet-4.5": { "name": "Overridden Built-in Sonnet" }
            }
        }}
    }));

    let (_runtime, registry) = harness.registry().await;
    let models = models_for_provider(&registry, "openrouter");

    assert!(
        models
            .iter()
            .any(|model| model.id == "custom/openrouter-model")
    );
    assert!(
        models
            .iter()
            .any(|model| model.id == "anthropic/claude-sonnet-4.5"
                && model.name == "Overridden Built-in Sonnet")
    );
}

#[tokio::test]
async fn refresh_reloads_merged_custom_models_from_disk() {
    let harness = Harness::new();
    harness.write_providers(json!({
        "anthropic": provider_json("https://first-proxy.example.com/v1", &["claude-custom"], "anthropic-messages")
    }));
    let (runtime, registry) = harness.registry().await;
    assert!(
        models_for_provider(&registry, "anthropic")
            .iter()
            .any(|model| model.id == "claude-custom")
    );

    harness.write_providers(json!({
        "anthropic": provider_json("https://second-proxy.example.com/v1", &["claude-custom-2"], "anthropic-messages")
    }));
    local_refresh(&runtime).await.expect("refresh");

    let anthropic = models_for_provider(&registry, "anthropic");
    assert!(!anthropic.iter().any(|model| model.id == "claude-custom"));
    assert!(anthropic.iter().any(|model| model.id == "claude-custom-2"));
    assert!(anthropic.iter().any(|model| model.id.contains("claude")));
}

#[tokio::test]
async fn removing_custom_models_keeps_builtin_provider_models() {
    let harness = Harness::new();
    harness.write_providers(json!({
        "anthropic": provider_json("https://proxy.example.com/v1", &["claude-custom"], "anthropic-messages")
    }));
    let (runtime, registry) = harness.registry().await;
    assert!(
        models_for_provider(&registry, "anthropic")
            .iter()
            .any(|model| model.id == "claude-custom")
    );

    harness.write_providers(json!({}));
    local_refresh(&runtime).await.expect("refresh");

    let anthropic = models_for_provider(&registry, "anthropic");
    assert!(!anthropic.iter().any(|model| model.id == "claude-custom"));
    assert!(anthropic.iter().any(|model| model.id.contains("claude")));
}

// ===========================================================================
// modelOverrides (per-model customization)
// ===========================================================================

#[tokio::test]
async fn model_override_applies_to_a_single_builtin_model() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "openrouter": { "modelOverrides": {
            "anthropic/claude-sonnet-4.5": { "name": "Custom Sonnet Name" }
        }}}
    }));

    let (_runtime, registry) = harness.registry().await;
    let models = models_for_provider(&registry, "openrouter");

    let sonnet = models
        .iter()
        .find(|model| model.id == "anthropic/claude-sonnet-4.5")
        .expect("sonnet");
    assert_eq!(sonnet.name, "Custom Sonnet Name");

    let opus = models
        .iter()
        .find(|model| model.id == "anthropic/claude-opus-4.1")
        .expect("opus");
    assert_ne!(opus.name, "Custom Sonnet Name");
}

#[tokio::test]
async fn anthropic_model_override_replaces_allowed_fallback_metadata() {
    let allowed = json!([
        { "provider": "anthropic", "model": "claude-opus-5", "cost": { "input": 5, "output": 25, "cacheRead": 0.5, "cacheWrite": 6.25 } },
        { "provider": "anthropic", "model": "claude-opus-4-8", "cost": { "input": 4, "output": 20, "cacheRead": 0.4, "cacheWrite": 5 } }
    ]);
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "anthropic": { "modelOverrides": {
            "claude-fable-5": { "compat": { "allowedFallbackModels": allowed } }
        }}}
    }));

    let (_runtime, registry) = harness.registry().await;
    let model = registry.find("anthropic", "claude-fable-5").expect("model");

    assert!(registry.get_error().is_none());
    assert_eq!(
        compat_value(&model, "allowedFallbackModels"),
        Some(&allowed)
    );
}

#[tokio::test]
async fn empty_allowed_fallback_model_override_disables_server_side_fallback() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "anthropic": { "modelOverrides": {
            "claude-fable-5": { "compat": { "allowedFallbackModels": [] } }
        }}}
    }));

    let (_runtime, registry) = harness.registry().await;
    let model = registry.find("anthropic", "claude-fable-5").expect("model");

    assert!(registry.get_error().is_none());
    assert_eq!(
        compat_value(&model, "allowedFallbackModels"),
        Some(&json!([]))
    );
}

#[tokio::test]
async fn custom_models_and_model_overrides_carry_sampling_params() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "openrouter": {
            "baseUrl": "https://my-proxy.example.com/v1",
            "api": "openai-completions",
            "models": [{
                "id": "custom/sampling-model",
                "samplingParams": { "temperature": 1, "top_p": 0.95, "top_k": 0 },
                "samplingParamsByThinkingLevel": {
                    "low": { "temperature": 0.6, "top_p": 0.95 },
                    "high": { "temperature": 0.8 }
                }
            }],
            "modelOverrides": {
                "custom/sampling-model": {
                    "samplingParamsByThinkingLevel": {
                        "low": { "temperature": 0.5, "top_k": 20 },
                        "max": { "temperature": 1 }
                    }
                },
                "anthropic/claude-sonnet-4.5": {
                    "samplingParams": { "top_p": 0.9 },
                    "samplingParamsByThinkingLevel": { "high": { "temperature": 0.8 } }
                }
            }
        }}
    }));

    let (_runtime, registry) = harness.registry().await;
    let models = models_for_provider(&registry, "openrouter");

    let custom = models
        .iter()
        .find(|model| model.id == "custom/sampling-model")
        .expect("custom");
    assert_eq!(
        custom.sampling_params,
        Some(json_object(
            json!({ "temperature": 1, "top_p": 0.95, "top_k": 0 })
        ))
    );
    let mut expected: IndexMap<ModelThinkingLevel, IndexMap<String, Value>> = IndexMap::new();
    expected.insert(
        ModelThinkingLevel::Low,
        json_object(json!({ "temperature": 0.5, "top_p": 0.95, "top_k": 20 })),
    );
    expected.insert(
        ModelThinkingLevel::High,
        json_object(json!({ "temperature": 0.8 })),
    );
    expected.insert(
        ModelThinkingLevel::Max,
        json_object(json!({ "temperature": 1 })),
    );
    assert_eq!(custom.sampling_params_by_thinking_level, Some(expected));

    let sonnet = models
        .iter()
        .find(|model| model.id == "anthropic/claude-sonnet-4.5")
        .expect("sonnet");
    assert_eq!(
        sonnet.sampling_params,
        Some(json_object(json!({ "top_p": 0.9 })))
    );
    let mut expected: IndexMap<ModelThinkingLevel, IndexMap<String, Value>> = IndexMap::new();
    expected.insert(
        ModelThinkingLevel::High,
        json_object(json!({ "temperature": 0.8 })),
    );
    assert_eq!(sonnet.sampling_params_by_thinking_level, Some(expected));

    let opus = models
        .iter()
        .find(|model| model.id == "anthropic/claude-opus-4.1")
        .expect("opus");
    assert!(opus.sampling_params.is_none());
    assert!(opus.sampling_params_by_thinking_level.is_none());
}

#[tokio::test]
async fn custom_model_and_model_override_carry_prompt_cache_lifetimes() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": {
            "openrouter": {
                "baseUrl": "https://my-proxy.example.com/v1",
                "api": "openai-completions",
                "models": [{ "id": "custom/cached-model", "promptCache": { "short": 120 } }],
                "modelOverrides": {
                    "anthropic/claude-sonnet-4.5": { "promptCache": { "short": 300 } }
                }
            },
            "anthropic": {
                "modelOverrides": {
                    "claude-sonnet-4-6": { "promptCache": { "long": 1800 } }
                }
            }
        }
    }));

    let (_runtime, registry) = harness.registry().await;
    let openrouter = models_for_provider(&registry, "openrouter");

    assert!(registry.get_error().is_none());
    let cache = |model: &Model| -> Option<ModelPromptCache> { model.prompt_cache.clone() };
    assert_eq!(
        cache(
            openrouter
                .iter()
                .find(|model| model.id == "custom/cached-model")
                .expect("custom")
        ),
        Some(IndexMap::from([(PromptCacheTier::Short, 120.0)]))
    );
    assert_eq!(
        cache(
            openrouter
                .iter()
                .find(|model| model.id == "anthropic/claude-sonnet-4.5")
                .expect("sonnet")
        ),
        Some(IndexMap::from([(PromptCacheTier::Short, 300.0)]))
    );
    assert!(
        cache(
            openrouter
                .iter()
                .find(|model| model.id == "anthropic/claude-opus-4.1")
                .expect("opus")
        )
        .is_none()
    );
    assert_eq!(
        cache(
            &registry
                .find("anthropic", "claude-sonnet-4-6")
                .expect("sonnet-4-6")
        ),
        Some(IndexMap::from([
            (PromptCacheTier::Short, 300.0),
            (PromptCacheTier::Long, 1800.0),
        ]))
    );
}

#[tokio::test]
async fn model_override_deep_merges_image_resize_limits() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "test": {
            "baseUrl": "https://example.com",
            "apiKey": "test-key",
            "api": "openai-completions",
            "models": [{
                "id": "vision-model",
                "input": ["text", "image"],
                "inputLimits": {
                    "maxRequestBytes": 33554432,
                    "images": {
                        "maxPerRequest": 100,
                        "resize": {
                            "maxWidth": 2000,
                            "maxHeight": 2000,
                            "maxBytes": 4718592,
                            "jpegQuality": 80
                        }
                    }
                }
            }],
            "modelOverrides": {
                "vision-model": {
                    "inputLimits": {
                        "images": { "resize": { "maxWidth": 1568, "maxBytes": 524288, "jpegQuality": 75 } }
                    }
                }
            }
        }}
    }));

    let (_runtime, registry) = harness.registry().await;
    let model = registry.find("test", "vision-model").expect("model");

    assert!(registry.get_error().is_none());
    let limits = model.input_limits.expect("inputLimits");
    assert_eq!(limits.max_request_bytes, Some(32 * 1024 * 1024));
    let images = limits.images.expect("images");
    assert_eq!(images.max_per_request, Some(100));
    let resize = images.resize.expect("resize");
    assert_eq!(resize.max_width, Some(1568));
    assert_eq!(resize.max_height, Some(2000));
    assert_eq!(resize.max_bytes, Some(524288));
    assert_eq!(resize.jpeg_quality, Some(75));
}

#[tokio::test]
async fn model_override_with_compat_open_router_routing() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "openrouter": { "modelOverrides": {
            "anthropic/claude-sonnet-4.5": {
                "compat": { "openRouterRouting": { "only": ["amazon-bedrock"] } }
            }
        }}}
    }));

    let (_runtime, registry) = harness.registry().await;
    let sonnet = models_for_provider(&registry, "openrouter")
        .into_iter()
        .find(|model| model.id == "anthropic/claude-sonnet-4.5")
        .expect("sonnet");

    assert_eq!(
        compat_value(&sonnet, "openRouterRouting"),
        Some(&json!({ "only": ["amazon-bedrock"] }))
    );
}

#[tokio::test]
async fn supports_finish_reason_can_be_configured_at_provider_and_model_levels() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "openrouter": {
            "compat": { "supportsFinishReason": true },
            "modelOverrides": {
                "anthropic/claude-sonnet-4.5": { "compat": { "supportsFinishReason": false } }
            }
        }}
    }));

    let (_runtime, registry) = harness.registry().await;
    let models = models_for_provider(&registry, "openrouter");
    let sonnet = models
        .iter()
        .find(|model| model.id == "anthropic/claude-sonnet-4.5")
        .expect("sonnet");
    let opus = models
        .iter()
        .find(|model| model.id == "anthropic/claude-opus-4.1")
        .expect("opus");

    assert_eq!(
        compat_value(sonnet, "supportsFinishReason"),
        Some(&json!(false))
    );
    assert_eq!(
        compat_value(opus, "supportsFinishReason"),
        Some(&json!(true))
    );
}

#[tokio::test]
async fn model_override_deep_merges_compat_settings() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "openrouter": { "modelOverrides": {
            "anthropic/claude-sonnet-4.5": {
                "compat": { "openRouterRouting": { "order": ["anthropic", "together"] } }
            }
        }}}
    }));

    let (_runtime, registry) = harness.registry().await;
    let sonnet = models_for_provider(&registry, "openrouter")
        .into_iter()
        .find(|model| model.id == "anthropic/claude-sonnet-4.5")
        .expect("sonnet");

    assert_eq!(
        compat_value(&sonnet, "openRouterRouting"),
        Some(&json!({ "order": ["anthropic", "together"] }))
    );
}

#[tokio::test]
async fn multiple_model_overrides_on_same_provider() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "openrouter": { "modelOverrides": {
            "anthropic/claude-sonnet-4.5": {
                "compat": { "openRouterRouting": { "only": ["amazon-bedrock"] } }
            },
            "anthropic/claude-opus-4.1": {
                "compat": { "openRouterRouting": { "only": ["anthropic"] } }
            }
        }}}
    }));

    let (_runtime, registry) = harness.registry().await;
    let models = models_for_provider(&registry, "openrouter");
    let sonnet = models
        .iter()
        .find(|model| model.id == "anthropic/claude-sonnet-4.5")
        .expect("sonnet");
    let opus = models
        .iter()
        .find(|model| model.id == "anthropic/claude-opus-4.1")
        .expect("opus");

    assert_eq!(
        compat_value(sonnet, "openRouterRouting"),
        Some(&json!({ "only": ["amazon-bedrock"] }))
    );
    assert_eq!(
        compat_value(opus, "openRouterRouting"),
        Some(&json!({ "only": ["anthropic"] }))
    );
}

#[tokio::test]
async fn model_override_combined_with_base_url_override() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "openrouter": {
            "baseUrl": "https://my-proxy.example.com/v1",
            "modelOverrides": {
                "anthropic/claude-sonnet-4.5": { "name": "Proxied Sonnet" }
            }
        }}
    }));

    let (_runtime, registry) = harness.registry().await;
    let models = models_for_provider(&registry, "openrouter");
    let sonnet = models
        .iter()
        .find(|model| model.id == "anthropic/claude-sonnet-4.5")
        .expect("sonnet");

    assert_eq!(sonnet.base_url, "https://my-proxy.example.com/v1");
    assert_eq!(sonnet.name, "Proxied Sonnet");

    let opus = models
        .iter()
        .find(|model| model.id == "anthropic/claude-opus-4.1")
        .expect("opus");
    assert_eq!(opus.base_url, "https://my-proxy.example.com/v1");
    assert_ne!(opus.name, "Proxied Sonnet");
}

#[tokio::test]
async fn model_override_for_nonexistent_model_id_is_ignored() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "openrouter": { "modelOverrides": {
            "nonexistent/model-id": { "name": "This should not appear" }
        }}}
    }));

    let (_runtime, registry) = harness.registry().await;
    let models = models_for_provider(&registry, "openrouter");

    assert!(
        !models
            .iter()
            .any(|model| model.id == "nonexistent/model-id")
    );
    assert!(registry.get_error().is_none());
}

#[tokio::test]
async fn model_override_can_change_cost_fields_partially() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "openrouter": { "modelOverrides": {
            "anthropic/claude-sonnet-4.5": { "cost": { "input": 99 } }
        }}}
    }));

    let (_runtime, registry) = harness.registry().await;
    let sonnet = models_for_provider(&registry, "openrouter")
        .into_iter()
        .find(|model| model.id == "anthropic/claude-sonnet-4.5")
        .expect("sonnet");

    assert_eq!(sonnet.cost.input, 99.0);
    assert!(sonnet.cost.output > 0.0);
}

#[tokio::test]
async fn model_override_can_add_headers_at_request_time() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "openrouter": { "modelOverrides": {
            "anthropic/claude-sonnet-4.5": {
                "headers": { "X-Custom-Model-Header": "value" }
            }
        }}}
    }));

    let (_runtime, registry) = harness.registry().await;
    let sonnet = models_for_provider(&registry, "openrouter")
        .into_iter()
        .find(|model| model.id == "anthropic/claude-sonnet-4.5")
        .expect("sonnet");

    let auth = registry.get_api_key_and_headers(&sonnet).await;
    let ResolvedRequestAuth::Ok(resolved) = auth else {
        panic!("expected ok");
    };
    let resolved_headers = resolved.headers.expect("headers");
    assert_eq!(
        header_value(&resolved_headers, "X-Custom-Model-Header"),
        Some(Some("value"))
    );
}

#[tokio::test]
async fn refresh_picks_up_model_override_changes() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "openrouter": { "modelOverrides": {
            "anthropic/claude-sonnet-4.5": { "name": "First Name" }
        }}}
    }));
    let (runtime, registry) = harness.registry().await;

    assert_eq!(
        models_for_provider(&registry, "openrouter")
            .into_iter()
            .find(|model| model.id == "anthropic/claude-sonnet-4.5")
            .expect("sonnet")
            .name,
        "First Name"
    );

    harness.write_raw(json!({
        "providers": { "openrouter": { "modelOverrides": {
            "anthropic/claude-sonnet-4.5": { "name": "Second Name" }
        }}}
    }));
    local_refresh(&runtime).await.expect("refresh");

    assert_eq!(
        models_for_provider(&registry, "openrouter")
            .into_iter()
            .find(|model| model.id == "anthropic/claude-sonnet-4.5")
            .expect("sonnet")
            .name,
        "Second Name"
    );
}

#[tokio::test]
async fn removing_model_override_restores_builtin_values() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "openrouter": { "modelOverrides": {
            "anthropic/claude-sonnet-4.5": { "name": "Custom Name" }
        }}}
    }));
    let (runtime, registry) = harness.registry().await;

    assert_eq!(
        models_for_provider(&registry, "openrouter")
            .into_iter()
            .find(|model| model.id == "anthropic/claude-sonnet-4.5")
            .expect("sonnet")
            .name,
        "Custom Name"
    );

    harness.write_raw(json!({}));
    local_refresh(&runtime).await.expect("refresh");

    assert_ne!(
        models_for_provider(&registry, "openrouter")
            .into_iter()
            .find(|model| model.id == "anthropic/claude-sonnet-4.5")
            .expect("sonnet")
            .name,
        "Custom Name"
    );
}

// ===========================================================================
// dynamic provider lifecycle
// ===========================================================================

#[tokio::test]
async fn get_provider_display_name_resolves_registered_oauth_builtin_and_fallback_names() {
    let harness = Harness::new();
    let (runtime, registry) = harness.registry().await;

    assert_eq!(registry.get_provider_display_name("openai"), "OpenAI");
    assert_eq!(
        registry.get_provider_display_name("github-copilot"),
        "GitHub Copilot"
    );
    assert_eq!(registry.get_provider_display_name("zai"), "Z.AI");
    assert_eq!(
        registry.get_provider_display_name("unknown-provider"),
        "unknown-provider"
    );

    let mut config = registered_provider(
        "https://provider.test/v1",
        "openai-completions",
        Some("test-key"),
        &[("demo-model", "Demo Model")],
    );
    config.name = Some("Named Provider".to_string());
    runtime
        .register_provider("named-provider", config)
        .expect("register");
    assert_eq!(
        registry.get_provider_display_name("named-provider"),
        "Named Provider"
    );

    runtime
        .register_provider(
            "oauth-provider",
            ProviderConfigInput {
                base_url: Some("https://provider.test/v1".to_string()),
                api: Some("openai-completions".to_string()),
                oauth: Some(extension_oauth("OAuth Provider", None)),
                models: Some(vec![chat_model_config("demo-model", "Demo Model")]),
                ..Default::default()
            },
        )
        .expect("register");
    assert_eq!(
        registry.get_provider_display_name("oauth-provider"),
        "OAuth Provider"
    );
}

#[tokio::test]
async fn model_overrides_apply_to_dynamically_registered_provider_models() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "extension-provider": { "modelOverrides": {
            "extension-model": {
                "name": "Overridden Extension Model",
                "thinkingLevelMap": {
                    "off": null, "minimal": null, "low": null, "medium": null, "xhigh": "max"
                },
                "headers": { "x-model-override": "enabled" }
            }
        }}}
    }));

    let (runtime, registry) = harness.registry().await;
    let mut config = registered_provider(
        "https://provider.test/v1",
        "openai-completions",
        Some("test-key"),
        &[("extension-model", "Extension Model")],
    );
    if let Some(models) = &mut config.models
        && let ProviderModelConfig::Chat(chat) = &mut models[0]
    {
        chat.reasoning = true;
    }
    runtime
        .register_provider("extension-provider", config)
        .expect("register");

    let model = registry
        .find("extension-provider", "extension-model")
        .expect("extension model was not registered");
    assert_eq!(model.name, "Overridden Extension Model");
    let map = model.thinking_level_map.clone().expect("thinkingLevelMap");
    assert_eq!(map.get(&ModelThinkingLevel::Off), Some(&None));
    assert_eq!(map.get(&ModelThinkingLevel::Minimal), Some(&None));
    assert_eq!(map.get(&ModelThinkingLevel::Low), Some(&None));
    assert_eq!(map.get(&ModelThinkingLevel::Medium), Some(&None));
    assert_eq!(
        map.get(&ModelThinkingLevel::Xhigh),
        Some(&Some("max".to_string()))
    );
    assert_eq!(
        get_supported_thinking_levels(&model),
        vec![ModelThinkingLevel::High, ModelThinkingLevel::Xhigh]
    );

    let auth = registry.get_api_key_and_headers(&model).await;
    let ResolvedRequestAuth::Ok(resolved) = auth else {
        panic!("expected ok");
    };
    let resolved_headers = resolved.headers.expect("headers");
    assert_eq!(
        header_value(&resolved_headers, "x-model-override"),
        Some(Some("enabled"))
    );
}

#[tokio::test]
async fn stored_api_key_env_propagates_to_request_auth_and_resolves_headers() {
    let harness = Harness::with_auth(json!({
        "cloudflare-ai-gateway": {
            "type": "api_key",
            "key": "$CLOUDFLARE_API_KEY",
            "env": {
                "CLOUDFLARE_API_KEY": "stored-cf-token",
                "CLOUDFLARE_ACCOUNT_ID": "stored-account",
                "CLOUDFLARE_GATEWAY_ID": "stored-gateway"
            }
        }
    }));
    harness.write_raw(json!({
        "providers": { "cloudflare-ai-gateway": {
            "headers": { "x-account": "$CLOUDFLARE_ACCOUNT_ID" }
        }}
    }));

    let (_runtime, registry) = harness.registry().await;
    let model = registry
        .get_all()
        .into_iter()
        .find(|model| model.provider == "cloudflare-ai-gateway")
        .expect("model");

    let auth = registry.get_api_key_and_headers(&model).await;

    assert_eq!(
        auth,
        ResolvedRequestAuth::Ok(ResolvedAuth {
            api_key: None,
            headers: Some(headers(&[
                ("cf-aig-authorization", Some("Bearer stored-cf-token")),
                ("Authorization", None),
                ("x-api-key", None),
                ("x-account", Some("stored-account")),
            ])),
            base_url: None,
            env: Some(IndexMap::from([
                (
                    "CLOUDFLARE_ACCOUNT_ID".to_string(),
                    "stored-account".to_string()
                ),
                (
                    "CLOUDFLARE_GATEWAY_ID".to_string(),
                    "stored-gateway".to_string()
                ),
            ])),
        })
    );
}

#[tokio::test]
async fn register_provider_treats_uppercase_api_key_and_headers_as_literals() {
    set_env("CUSTOM_NAME", "env-CUSTOM_NAME");
    set_env("BEARER", "env-BEARER");
    set_env("MODEL_TOKEN", "env-MODEL_TOKEN");

    let result = async {
        let harness = Harness::new();
        let (runtime, registry) = harness.registry().await;

        let mut config = registered_provider(
            "https://provider.test/v1",
            "openai-completions",
            Some("CUSTOM_NAME"),
            &[("demo-model", "demo-model")],
        );
        config.headers = Some(IndexMap::from([(
            "Authorization".to_string(),
            "BEARER".to_string(),
        )]));
        if let Some(models) = &mut config.models
            && let ProviderModelConfig::Chat(chat) = &mut models[0]
        {
            chat.base.headers = Some(IndexMap::from([(
                "x-model-token".to_string(),
                "MODEL_TOKEN".to_string(),
            )]));
        }
        runtime
            .register_provider("literal-provider", config)
            .expect("register");

        assert_eq!(
            registry.get_api_key_for_provider("literal-provider").await,
            Some("CUSTOM_NAME".to_string())
        );
        let model = registry
            .find("literal-provider", "demo-model")
            .expect("model");
        let auth = registry.get_api_key_and_headers(&model).await;
        let ResolvedRequestAuth::Ok(resolved) = auth else {
            panic!("expected ok");
        };
        assert_eq!(resolved.api_key, Some("CUSTOM_NAME".to_string()));
        let resolved_headers = resolved.headers.expect("headers");
        assert_eq!(
            header_value(&resolved_headers, "Authorization"),
            Some(Some("BEARER"))
        );
        assert_eq!(
            header_value(&resolved_headers, "x-model-token"),
            Some(Some("MODEL_TOKEN"))
        );
    }
    .await;

    remove_env("CUSTOM_NAME");
    remove_env("BEARER");
    remove_env("MODEL_TOKEN");
    result
}

/// 回归：TS 的 `effective.oauth && !effective.apiKey`——空串 apiKey 是 falsy，注册时的
/// 临时 auth 状态按 OAuth 记录（修复前按 api_key 记录，`isUsingOAuth` 直到下一轮可用性
/// 刷新前都不一致）。
#[tokio::test]
async fn register_provider_treats_an_empty_api_key_as_missing_for_the_provisional_auth_type() {
    let harness = Harness::new();
    let (runtime, registry) = harness.registry().await;

    runtime
        .register_provider(
            "oauth-empty-key",
            ProviderConfigInput {
                base_url: Some("https://provider.test/v1".to_string()),
                api: Some("openai-completions".to_string()),
                api_key: Some(String::new()),
                oauth: Some(extension_oauth("OAuth Provider", None)),
                models: Some(vec![chat_model_config("demo-model", "Demo Model")]),
                ..Default::default()
            },
        )
        .expect("register");

    let model = registry
        .find("oauth-empty-key", "demo-model")
        .expect("registered model");
    assert!(registry.is_using_oauth(&model));
}

#[tokio::test]
async fn failed_register_provider_does_not_persist_invalid_stream_simple_config() {
    let harness = Harness::new();
    let (runtime, registry) = harness.registry().await;

    let error = runtime
        .register_provider(
            "broken-provider",
            ProviderConfigInput {
                stream_simple: Some(Arc::new(
                    |_model: &Model,
                     _context: &TranscriptContext,
                     _options: SimpleStreamOptions| {
                        kiana_ai::event_stream::AssistantMessageEventStream::new()
                    },
                )),
                ..Default::default()
            },
        )
        .expect_err("register should fail");
    assert_eq!(
        error.message(),
        "Provider broken-provider: \"api\" is required when registering streamSimple."
    );

    let result = registry
        .refresh(kiana_ai::models::ModelsRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        })
        .await
        .expect("refresh");
    assert!(!result.aborted);
}

#[tokio::test]
async fn failed_register_provider_does_not_remove_existing_provider_models() {
    let harness = Harness::new();
    let (runtime, registry) = harness.registry().await;

    runtime
        .register_provider(
            "demo-provider",
            registered_provider(
                "https://provider.test/v1",
                "openai-completions",
                Some("test-key"),
                &[("demo-model", "Demo Model")],
            ),
        )
        .expect("register");
    assert!(registry.find("demo-provider", "demo-model").is_some());

    let error = runtime
        .register_provider(
            "demo-provider",
            ProviderConfigInput {
                base_url: Some("https://provider.test/v2".to_string()),
                api_key: Some("test-key".to_string()),
                models: Some(vec![chat_model_config("broken-model", "Broken Model")]),
                ..Default::default()
            },
        )
        .expect_err("register should fail");
    assert!(
        error
            .message()
            .contains("Provider demo-provider, model broken-model: no \"api\" specified")
    );

    assert!(registry.find("demo-provider", "demo-model").is_some());
    let result = registry
        .refresh(kiana_ai::models::ModelsRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        })
        .await
        .expect("refresh");
    assert!(!result.aborted);
    assert!(registry.find("demo-provider", "demo-model").is_some());
}

/// §3.2 的逐字错误：`registerProvider(name, undefined)`。
#[tokio::test]
async fn registering_by_name_without_a_config_matches_the_typescript_error() {
    let harness = Harness::new();
    let (_runtime, registry) = harness.registry().await;

    let error = registry
        .register_provider("demo-provider", None)
        .expect_err("missing config");
    assert_eq!(
        error.message(),
        "Provider config is required when registering by name"
    );
}

/// §3.2 的逐字错误：`registerProvider(<Provider with empty id>)`。
#[tokio::test]
async fn registering_a_native_provider_with_an_empty_id_fails() {
    let harness = Harness::new();
    let (runtime, registry) = harness.registry().await;

    let auth = Arc::new(TestApiKeyAuth::new("API key")).provider_auth();
    let error = registry
        .register_native_provider(TestProvider::new("", auth, Vec::new()).into_provider())
        .expect_err("empty id");
    assert_eq!(error.message(), "Provider id must not be empty.");

    let auth = Arc::new(TestApiKeyAuth::new("API key")).provider_auth();
    let error = runtime
        .register_native_provider(TestProvider::new("", auth, Vec::new()).into_provider())
        .expect_err("empty id");
    assert_eq!(error.message(), "Provider id must not be empty.");
    assert!(registry.get_registered_provider_ids().is_empty());
}

#[tokio::test]
async fn unregister_provider_removes_the_runtime_oauth_overlay() {
    let harness = Harness::new();
    let (runtime, registry) = harness.registry().await;

    runtime
        .register_provider(
            "anthropic",
            ProviderConfigInput {
                oauth: Some(extension_oauth("Custom Anthropic OAuth", None)),
                ..Default::default()
            },
        )
        .expect("register");

    assert_eq!(
        registry
            .get_registered_provider_config("anthropic")
            .and_then(|config| config.oauth.map(|oauth| oauth.name)),
        Some("Custom Anthropic OAuth".to_string())
    );

    registry.unregister_provider("anthropic");

    assert!(
        registry
            .get_registered_provider_config("anthropic")
            .is_none()
    );
}

#[tokio::test]
async fn stream_simple_overlays_do_not_mutate_the_global_compat_api_registry() {
    let harness = Harness::new();
    let (runtime, _registry) = harness.registry().await;

    runtime
        .register_provider(
            "stream-override-provider",
            ProviderConfigInput {
                api: Some("openai-completions".to_string()),
                stream_simple: Some(Arc::new(
                    |_model: &Model,
                     _context: &TranscriptContext,
                     _options: SimpleStreamOptions| {
                        kiana_ai::event_stream::AssistantMessageEventStream::new()
                    },
                )),
                ..Default::default()
            },
        )
        .expect("register");

    let before = runtime.get_provider("openai").expect("openai provider");
    let _stream = before.stream_simple(
        &chat_model("openai", "test-openai-model"),
        &TranscriptContext::from_normalized(kiana_ai::types::Context::default()),
        SimpleStreamOptions::default(),
    );

    runtime.unregister_provider("stream-override-provider");

    let after = runtime.get_provider("openai").expect("openai provider");
    assert!(Arc::ptr_eq(&before, &after));
    let _stream = after.stream_simple(
        &chat_model("openai", "test-openai-model"),
        &TranscriptContext::from_normalized(kiana_ai::types::Context::default()),
        SimpleStreamOptions::default(),
    );
}

#[tokio::test]
async fn base_url_only_override_keeps_builtin_provider_models_after_refresh() {
    let harness = Harness::new();
    let (runtime, registry) = harness.registry().await;

    runtime
        .register_provider(
            "anthropic",
            ProviderConfigInput {
                base_url: Some("https://proxy.test/anthropic".to_string()),
                ..Default::default()
            },
        )
        .expect("register");
    local_refresh(&runtime).await.expect("refresh");

    let anthropic = models_for_provider(&registry, "anthropic");
    assert!(anthropic.len() > 1);
    assert!(
        anthropic
            .iter()
            .all(|model| model.base_url == "https://proxy.test/anthropic")
    );
}

#[tokio::test]
async fn models_only_override_replaces_builtin_provider_models_after_refresh() {
    let harness = Harness::new();
    let (runtime, registry) = harness.registry().await;

    runtime
        .register_provider(
            "anthropic",
            registered_provider(
                "https://custom.test/anthropic",
                "anthropic-messages",
                Some("test-key"),
                &[("custom-claude", "custom-claude")],
            ),
        )
        .expect("register");
    local_refresh(&runtime).await.expect("refresh");

    assert_eq!(
        ids(&models_for_provider(&registry, "anthropic")),
        vec!["custom-claude".to_string()]
    );
    assert_eq!(
        registry
            .find("anthropic", "custom-claude")
            .expect("model")
            .base_url,
        "https://custom.test/anthropic"
    );
}

#[tokio::test]
async fn models_plus_base_url_override_replaces_builtin_provider_models_after_refresh() {
    let harness = Harness::new();
    let (runtime, registry) = harness.registry().await;

    runtime
        .register_provider(
            "anthropic",
            registered_provider(
                "https://custom.test/anthropic",
                "anthropic-messages",
                Some("test-key"),
                &[("custom-claude", "custom-claude")],
            ),
        )
        .expect("register");
    runtime
        .register_provider(
            "anthropic",
            ProviderConfigInput {
                base_url: Some("https://proxy.test/anthropic".to_string()),
                ..Default::default()
            },
        )
        .expect("register");
    local_refresh(&runtime).await.expect("refresh");

    assert_eq!(
        ids(&models_for_provider(&registry, "anthropic")),
        vec!["custom-claude".to_string()]
    );
    assert_eq!(
        registry
            .find("anthropic", "custom-claude")
            .expect("model")
            .base_url,
        "https://proxy.test/anthropic"
    );
}

#[tokio::test]
async fn models_only_custom_provider_registration_survives_refresh() {
    let harness = Harness::new();
    let (runtime, registry) = harness.registry().await;

    runtime
        .register_provider(
            "custom-provider",
            registered_provider(
                "https://custom.test/v1",
                "openai-completions",
                Some("test-key"),
                &[("custom-a", "custom-a"), ("custom-b", "custom-b")],
            ),
        )
        .expect("register");
    local_refresh(&runtime).await.expect("refresh");

    assert_eq!(
        ids(&models_for_provider(&registry, "custom-provider")),
        vec!["custom-a".to_string(), "custom-b".to_string()]
    );
}

#[tokio::test]
async fn base_url_only_override_keeps_custom_provider_models_after_refresh() {
    let harness = Harness::new();
    let (runtime, registry) = harness.registry().await;

    runtime
        .register_provider(
            "custom-provider",
            registered_provider(
                "https://custom.test/v1",
                "openai-completions",
                Some("test-key"),
                &[("custom-a", "custom-a"), ("custom-b", "custom-b")],
            ),
        )
        .expect("register");
    runtime
        .register_provider(
            "custom-provider",
            ProviderConfigInput {
                base_url: Some("https://proxy.test/custom".to_string()),
                ..Default::default()
            },
        )
        .expect("register");
    local_refresh(&runtime).await.expect("refresh");

    let models = models_for_provider(&registry, "custom-provider");
    assert_eq!(
        ids(&models),
        vec!["custom-a".to_string(), "custom-b".to_string()]
    );
    assert!(
        models
            .iter()
            .all(|model| model.base_url == "https://proxy.test/custom")
    );
}

#[tokio::test]
async fn headers_only_override_keeps_custom_provider_models_after_refresh() {
    let harness = Harness::new();
    let (runtime, registry) = harness.registry().await;

    runtime
        .register_provider(
            "custom-provider",
            registered_provider(
                "https://custom.test/v1",
                "openai-completions",
                Some("test-key"),
                &[("custom-a", "custom-a"), ("custom-b", "custom-b")],
            ),
        )
        .expect("register");
    runtime
        .register_provider(
            "custom-provider",
            ProviderConfigInput {
                headers: Some(IndexMap::from([(
                    "x-proxy".to_string(),
                    "enabled".to_string(),
                )])),
                ..Default::default()
            },
        )
        .expect("register");
    local_refresh(&runtime).await.expect("refresh");

    let models = models_for_provider(&registry, "custom-provider");
    assert_eq!(
        ids(&models),
        vec!["custom-a".to_string(), "custom-b".to_string()]
    );
    assert!(
        models
            .iter()
            .all(|model| model.base_url == "https://custom.test/v1")
    );
    let auth = registry.get_api_key_and_headers(&models[0]).await;
    let ResolvedRequestAuth::Ok(resolved) = auth else {
        panic!("expected ok");
    };
    let resolved_headers = resolved.headers.expect("headers");
    assert_eq!(
        header_value(&resolved_headers, "x-proxy"),
        Some(Some("enabled"))
    );
}

// ===========================================================================
// API key resolution
// ===========================================================================

#[tokio::test]
async fn api_key_with_bang_prefix_executes_command_and_uses_stdout() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "custom-provider": provider_with_api_key("!echo test-api-key-from-command") }
    }));

    let (_runtime, registry) = harness.registry().await;
    assert_eq!(
        registry.get_api_key_for_provider("custom-provider").await,
        Some("test-api-key-from-command".to_string())
    );
}

#[tokio::test]
async fn api_key_with_bang_prefix_trims_whitespace_from_command_output() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "custom-provider": provider_with_api_key("!echo '  spaced-key  '") }
    }));

    let (_runtime, registry) = harness.registry().await;
    assert_eq!(
        registry.get_api_key_for_provider("custom-provider").await,
        Some("spaced-key".to_string())
    );
}

#[tokio::test]
async fn api_key_with_bang_prefix_handles_multiline_output() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "custom-provider": provider_with_api_key("!printf 'line1\\nline2'") }
    }));

    let (_runtime, registry) = harness.registry().await;
    assert_eq!(
        registry.get_api_key_for_provider("custom-provider").await,
        Some("line1\nline2".to_string())
    );
}

#[tokio::test]
async fn api_key_with_bang_prefix_returns_none_on_command_failure() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "custom-provider": provider_with_api_key("!exit 1") }
    }));

    let (_runtime, registry) = harness.registry().await;
    assert_eq!(
        registry.get_api_key_for_provider("custom-provider").await,
        None
    );
}

#[tokio::test]
async fn api_key_with_bang_prefix_returns_none_on_nonexistent_command() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "custom-provider": provider_with_api_key("!nonexistent-command-12345") }
    }));

    let (_runtime, registry) = harness.registry().await;
    assert_eq!(
        registry.get_api_key_for_provider("custom-provider").await,
        None
    );
}

#[tokio::test]
async fn api_key_with_bang_prefix_returns_none_on_empty_output() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "custom-provider": provider_with_api_key("!printf ''") }
    }));

    let (_runtime, registry) = harness.registry().await;
    assert_eq!(
        registry.get_api_key_for_provider("custom-provider").await,
        None
    );
}

#[tokio::test]
async fn api_key_with_dollar_prefix_resolves_to_env_value() {
    set_env("TEST_API_KEY_E7_12345", "env-api-key-value");
    let result = async {
        let harness = Harness::new();
        harness.write_raw(json!({
            "providers": { "custom-provider": provider_with_api_key("$TEST_API_KEY_E7_12345") }
        }));
        let (_runtime, registry) = harness.registry().await;
        registry.get_api_key_for_provider("custom-provider").await
    }
    .await;
    remove_env("TEST_API_KEY_E7_12345");

    assert_eq!(result, Some("env-api-key-value".to_string()));
}

#[tokio::test]
async fn api_key_with_braced_env_syntax_resolves_to_env_value() {
    set_env("TEST_BRACED_API_KEY_E8_12345", "braced-env-api-key-value");
    let result = async {
        let harness = Harness::new();
        harness.write_raw(json!({
            "providers": { "custom-provider": provider_with_api_key("${TEST_BRACED_API_KEY_E8_12345}") }
        }));
        let (_runtime, registry) = harness.registry().await;
        registry.get_api_key_for_provider("custom-provider").await
    }
    .await;
    remove_env("TEST_BRACED_API_KEY_E8_12345");

    assert_eq!(result, Some("braced-env-api-key-value".to_string()));
}

#[tokio::test]
async fn api_key_interpolates_braced_env_references_inside_literals() {
    set_env("TEST_INTERPOLATED_PART_A_E9_12345", "left");
    set_env("TEST_INTERPOLATED_PART_B_E9_12345", "right");
    let result = async {
        let harness = Harness::new();
        harness.write_raw(json!({
            "providers": { "custom-provider": provider_with_api_key(
                "${TEST_INTERPOLATED_PART_A_E9_12345}_${TEST_INTERPOLATED_PART_B_E9_12345}"
            ) }
        }));
        let (_runtime, registry) = harness.registry().await;
        registry.get_api_key_for_provider("custom-provider").await
    }
    .await;
    remove_env("TEST_INTERPOLATED_PART_A_E9_12345");
    remove_env("TEST_INTERPOLATED_PART_B_E9_12345");

    assert_eq!(result, Some("left_right".to_string()));
}

#[tokio::test]
async fn api_key_with_double_dollar_prefix_escapes_a_leading_dollar() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "custom-provider": provider_with_api_key("$$TEST_API_KEY_E10_12345") }
    }));

    let (_runtime, registry) = harness.registry().await;
    assert_eq!(
        registry.get_api_key_for_provider("custom-provider").await,
        Some("$TEST_API_KEY_E10_12345".to_string())
    );
}

#[tokio::test]
async fn api_key_with_dollar_bang_escapes_a_literal_bang_and_still_interpolates() {
    set_env("TEST_API_KEY_E11_12345", "env-api-key-value");
    let result = async {
        let harness = Harness::new();
        harness.write_raw(json!({
            "providers": { "custom-provider": provider_with_api_key("$!literal-$TEST_API_KEY_E11_12345") }
        }));
        let (_runtime, registry) = harness.registry().await;
        registry.get_api_key_for_provider("custom-provider").await
    }
    .await;
    remove_env("TEST_API_KEY_E11_12345");

    assert_eq!(result, Some("!literal-env-api-key-value".to_string()));
}

#[tokio::test]
async fn plain_api_key_is_used_directly_even_when_it_matches_an_env_var() {
    set_env("TEST_API_KEY_E12_12345", "env-api-key-value");
    let result = async {
        let harness = Harness::new();
        harness.write_raw(json!({
            "providers": { "custom-provider": provider_with_api_key("TEST_API_KEY_E12_12345") }
        }));
        let (_runtime, registry) = harness.registry().await;
        registry.get_api_key_for_provider("custom-provider").await
    }
    .await;
    remove_env("TEST_API_KEY_E12_12345");

    assert_eq!(result, Some("TEST_API_KEY_E12_12345".to_string()));
}

#[tokio::test]
async fn api_key_as_literal_value_is_used_directly_when_not_an_env_var() {
    remove_env("literal_api_key_value");
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "custom-provider": provider_with_api_key("literal_api_key_value") }
    }));

    let (_runtime, registry) = harness.registry().await;
    assert_eq!(
        registry.get_api_key_for_provider("custom-provider").await,
        Some("literal_api_key_value".to_string())
    );
}

#[tokio::test]
async fn api_key_command_can_use_shell_features_like_pipes() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "custom-provider": provider_with_api_key("!echo 'hello world' | tr ' ' '-'") }
    }));

    let (_runtime, registry) = harness.registry().await;
    assert_eq!(
        registry.get_api_key_for_provider("custom-provider").await,
        Some("hello-world".to_string())
    );
}

#[tokio::test]
async fn api_key_command_is_executed_on_every_provider_lookup() {
    let harness = Harness::new();
    let counter = harness.dir().join("counter");
    std::fs::write(&counter, "0").expect("counter");
    let counter_path = counter.display();
    let command = format!(
        "!sh -c 'count=$(cat \"{counter_path}\"); echo $((count + 1)) > \"{counter_path}\"; echo \"key-value\"'"
    );
    harness.write_raw(json!({
        "providers": { "custom-provider": provider_with_api_key(&command) }
    }));

    let (_runtime, registry) = harness.registry().await;
    for _ in 0..3 {
        registry.get_api_key_for_provider("custom-provider").await;
    }

    let count: u32 = std::fs::read_to_string(&counter)
        .expect("counter")
        .trim()
        .parse()
        .expect("count");
    assert_eq!(count, 3);
}

#[tokio::test]
async fn api_key_commands_are_re_executed_across_registry_instances() {
    let harness = Harness::new();
    let counter = harness.dir().join("counter");
    std::fs::write(&counter, "0").expect("counter");
    let counter_path = counter.display();
    let command = format!(
        "!sh -c 'count=$(cat \"{counter_path}\"); echo $((count + 1)) > \"{counter_path}\"; echo \"key-value\"'"
    );
    harness.write_raw(json!({
        "providers": { "custom-provider": provider_with_api_key(&command) }
    }));

    let (_runtime1, registry1) = harness.registry().await;
    registry1.get_api_key_for_provider("custom-provider").await;

    let (_runtime2, registry2) = harness.registry().await;
    registry2.get_api_key_for_provider("custom-provider").await;

    let count: u32 = std::fs::read_to_string(&counter)
        .expect("counter")
        .trim()
        .parse()
        .expect("count");
    assert_eq!(count, 2);
}

#[tokio::test]
async fn different_api_key_commands_resolve_independently() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": {
            "provider-a": provider_with_api_key("!echo key-a"),
            "provider-b": provider_with_api_key("!echo key-b")
        }
    }));

    let (_runtime, registry) = harness.registry().await;
    let key_a = registry.get_api_key_for_provider("provider-a").await;
    let key_b = registry.get_api_key_for_provider("provider-b").await;

    assert_eq!(key_a, Some("key-a".to_string()));
    assert_eq!(key_b, Some("key-b".to_string()));
}

#[tokio::test]
async fn failed_api_key_commands_are_retried() {
    let harness = Harness::new();
    let counter = harness.dir().join("counter");
    std::fs::write(&counter, "0").expect("counter");
    let counter_path = counter.display();
    let command = format!(
        "!sh -c 'count=$(cat \"{counter_path}\"); echo $((count + 1)) > \"{counter_path}\"; exit 1'"
    );
    harness.write_raw(json!({
        "providers": { "custom-provider": provider_with_api_key(&command) }
    }));

    let (_runtime, registry) = harness.registry().await;
    let key1 = registry.get_api_key_for_provider("custom-provider").await;
    let key2 = registry.get_api_key_for_provider("custom-provider").await;

    assert_eq!(key1, None);
    assert_eq!(key2, None);

    let count: u32 = std::fs::read_to_string(&counter)
        .expect("counter")
        .trim()
        .parse()
        .expect("count");
    assert_eq!(count, 2);
}

#[tokio::test]
async fn provider_auth_status_reports_api_key_environment_variables() {
    let env_var = "TEST_API_KEY_STATUS_TEST_E19_98765";
    set_env(env_var, "status-test-key");
    let result = async {
        let harness = Harness::new();
        harness.write_raw(json!({
            "providers": { "custom-provider": provider_with_api_key(&format!("${env_var}")) }
        }));
        let (_runtime, registry) = harness.registry().await;
        registry.get_provider_auth_status("custom-provider")
    }
    .await;
    remove_env(env_var);

    assert_eq!(
        result,
        AuthStatus {
            configured: true,
            source: Some(AuthStatusSource::Environment),
            label: Some(env_var.to_string()),
        }
    );
}

#[tokio::test]
async fn provider_auth_status_reports_interpolated_api_key_environment_variables() {
    let env_a = "TEST_API_KEY_STATUS_PART_A_E20_98765";
    let env_b = "TEST_API_KEY_STATUS_PART_B_E20_98765";
    set_env(env_a, "left");
    set_env(env_b, "right");
    let result = async {
        let harness = Harness::new();
        harness.write_raw(json!({
            "providers": { "custom-provider": provider_with_api_key(&format!("${{{env_a}}}_${{{env_b}}}")) }
        }));
        let (_runtime, registry) = harness.registry().await;
        registry.get_provider_auth_status("custom-provider")
    }
    .await;
    remove_env(env_a);
    remove_env(env_b);

    assert_eq!(
        result,
        AuthStatus {
            configured: true,
            source: Some(AuthStatusSource::Environment),
            label: Some(format!("{env_a}, {env_b}")),
        }
    );
}

#[tokio::test]
async fn provider_auth_status_reports_non_env_api_key_values_as_a_config_key() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "custom-provider": provider_with_api_key("literal_api_key_value") }
    }));

    let (_runtime, registry) = harness.registry().await;
    assert_eq!(
        registry.get_provider_auth_status("custom-provider"),
        AuthStatus {
            configured: true,
            source: Some(AuthStatusSource::ModelsJsonKey),
            label: None,
        }
    );
}

#[tokio::test]
async fn missing_explicit_env_api_key_keeps_provider_unavailable() {
    let env_var = "TEST_API_KEY_MISSING_E22_98765";
    remove_env(env_var);
    let result = async {
        let harness = Harness::new();
        harness.write_raw(json!({
            "providers": { "custom-provider": provider_with_api_key(&format!("${env_var}")) }
        }));
        let (_runtime, registry) = harness.registry().await;
        (
            registry.get_provider_auth_status("custom-provider"),
            registry.get_available(),
        )
    }
    .await;

    assert_eq!(
        result.0,
        AuthStatus {
            configured: false,
            source: None,
            label: None,
        }
    );
    assert!(
        !result
            .1
            .iter()
            .any(|model| model.provider == "custom-provider")
    );
}

#[tokio::test]
async fn provider_auth_status_reports_command_api_key_values_without_executing_them() {
    let harness = Harness::new();
    let counter = harness.dir().join("status-counter");
    std::fs::write(&counter, "0").expect("counter");
    let counter_path = counter.display();
    let command = format!("!sh -c 'echo 1 > \"{counter_path}\"; echo key-value'");
    harness.write_raw(json!({
        "providers": { "custom-provider": provider_with_api_key(&command) }
    }));

    let (_runtime, registry) = harness.registry().await;

    assert_eq!(
        registry.get_provider_auth_status("custom-provider"),
        AuthStatus {
            configured: true,
            source: Some(AuthStatusSource::ModelsJsonCommand),
            label: None,
        }
    );
    assert_eq!(
        std::fs::read_to_string(&counter).expect("counter"),
        "0".to_string()
    );
}

#[tokio::test]
async fn environment_variables_are_not_cached() {
    let env_var = "TEST_API_KEY_CACHE_E24_98765";
    set_env(env_var, "first-value");
    let result = async {
        let harness = Harness::new();
        harness.write_raw(json!({
            "providers": { "custom-provider": provider_with_api_key(&format!("${env_var}")) }
        }));
        let (_runtime, registry) = harness.registry().await;

        let key1 = registry.get_api_key_for_provider("custom-provider").await;
        set_env(env_var, "second-value");
        let key2 = registry.get_api_key_for_provider("custom-provider").await;
        (key1, key2)
    }
    .await;
    remove_env(env_var);

    assert_eq!(result.0, Some("first-value".to_string()));
    assert_eq!(result.1, Some("second-value".to_string()));
}

#[tokio::test]
async fn get_available_does_not_execute_command_backed_api_key_resolution() {
    let harness = Harness::new();
    let counter = harness.dir().join("counter");
    std::fs::write(&counter, "0").expect("counter");
    let counter_path = counter.display();
    let command = format!(
        "!sh -c 'count=$(cat \"{counter_path}\"); echo $((count + 1)) > \"{counter_path}\"; echo \"key-value\"'"
    );
    harness.write_raw(json!({
        "providers": { "custom-provider": provider_with_api_key(&command) }
    }));

    let (_runtime, registry) = harness.registry().await;
    let available = registry.get_available();

    assert!(
        available
            .iter()
            .any(|model| model.provider == "custom-provider")
    );
    let count: u32 = std::fs::read_to_string(&counter)
        .expect("counter")
        .trim()
        .parse()
        .expect("count");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn get_available_filters_github_copilot_oauth_models_to_account_picker_availability() {
    let copilot_model = kiana_ai::compat::get_models("github-copilot")
        .into_iter()
        .next()
        .expect("at least one GitHub Copilot model");

    let harness = Harness::with_auth(json!({
        "github-copilot": {
            "type": "oauth",
            "refresh": "github-access-token",
            "access": "tid=test;exp=9999999999;proxy-ep=proxy.individual.githubcopilot.com;",
            "expires": now_ms() + 60_000,
            "availableModelIds": [copilot_model.id]
        }
    }));
    let (_runtime, registry) = harness.registry().await;

    let ids: Vec<String> = registry
        .get_available()
        .into_iter()
        .filter(|model| model.provider == "github-copilot")
        .map(|model| model.id)
        .collect();
    assert_eq!(ids, vec![copilot_model.id]);
}

#[tokio::test]
async fn get_api_key_and_headers_resolves_auth_header_on_every_request() {
    let harness = Harness::new();
    let token_file = harness.dir().join("token");
    std::fs::write(&token_file, "token-1").expect("token");
    let token_path = token_file.display();
    let command = format!("!sh -c 'cat \"{token_path}\"'");
    let mut config = provider_with_api_key(&command);
    config["authHeader"] = json!(true);
    harness.write_raw(json!({ "providers": { "custom-provider": config } }));

    let (_runtime, registry) = harness.registry().await;
    let model = registry
        .find("custom-provider", "test-model")
        .expect("model");

    let auth1 = registry.get_api_key_and_headers(&model).await;
    assert_eq!(
        auth1,
        ResolvedRequestAuth::Ok(ResolvedAuth {
            api_key: Some("token-1".to_string()),
            headers: Some(headers(&[("Authorization", Some("Bearer token-1"))])),
            base_url: None,
            env: None,
        })
    );

    std::fs::write(&token_file, "token-2").expect("token");

    let auth2 = registry.get_api_key_and_headers(&model).await;
    assert_eq!(
        auth2,
        ResolvedRequestAuth::Ok(ResolvedAuth {
            api_key: Some("token-2".to_string()),
            headers: Some(headers(&[("Authorization", Some("Bearer token-2"))])),
            base_url: None,
            env: None,
        })
    );
}

#[tokio::test]
async fn get_api_key_and_headers_resolves_configured_auth_exactly_once() {
    let harness = Harness::new();
    let counter = harness.dir().join("auth-counter");
    std::fs::write(&counter, "0").expect("counter");
    let counter_path = counter.display();
    let command = format!(
        "!sh -c 'count=$(cat \"{counter_path}\"); count=$((count + 1)); echo \"$count\" > \"{counter_path}\"; echo \"token-$count\"'"
    );
    let mut config = provider_with_api_key(&command);
    config["authHeader"] = json!(true);
    harness.write_raw(json!({ "providers": { "custom-provider": config } }));

    let (_runtime, registry) = harness.registry().await;
    let model = registry
        .find("custom-provider", "test-model")
        .expect("model");
    let auth = registry.get_api_key_and_headers(&model).await;

    assert_eq!(
        auth,
        ResolvedRequestAuth::Ok(ResolvedAuth {
            api_key: Some("token-1".to_string()),
            headers: Some(headers(&[("Authorization", Some("Bearer token-1"))])),
            base_url: None,
            env: None,
        })
    );
    assert_eq!(
        std::fs::read_to_string(&counter).expect("counter").trim(),
        "1"
    );
}

/// 回归：TS 的成功分支是 `...(resolution.auth.baseUrl ? { baseUrl } : {})`——空串 baseUrl
/// 是 falsy，不出现在结果里（消费方按 `baseUrl !== undefined` 判断覆盖）。
#[tokio::test]
async fn get_api_key_and_headers_omits_an_empty_auth_base_url() {
    let harness = Harness::new();
    let (runtime, registry) = harness.registry().await;

    let auth = TestApiKeyAuth::new("Empty base URL key").with_resolve(Arc::new(|_credential| {
        Box::pin(async {
            Ok(Some(kiana_ai::auth::types::AuthResult::new(
                kiana_ai::auth::types::ModelAuth {
                    api_key: Some("key".to_string()),
                    base_url: Some(String::new()),
                    ..Default::default()
                },
            )))
        })
    }));
    runtime
        .register_native_provider(
            TestProvider::new(
                "empty-base-url",
                Arc::new(auth).provider_auth(),
                vec![chat_model("empty-base-url", "demo")],
            )
            .into_provider(),
        )
        .expect("register");

    let model = registry.find("empty-base-url", "demo").expect("model");
    let auth = registry.get_api_key_and_headers(&model).await;
    let ResolvedRequestAuth::Ok(resolved) = auth else {
        panic!("expected ok");
    };
    assert_eq!(resolved.api_key.as_deref(), Some("key"));
    assert_eq!(resolved.base_url, None);
}

#[tokio::test]
async fn stored_credentials_bypass_lower_priority_configured_auth_commands() {
    let harness = Harness::with_auth(json!({
        "custom-provider": api_key_credential("stored-key")
    }));
    let counter = harness.dir().join("fallback-counter");
    std::fs::write(&counter, "0").expect("counter");
    let counter_path = counter.display();
    let command = format!("!sh -c 'echo 1 > \"{counter_path}\"; echo fallback-key'");
    harness.write_raw(json!({
        "providers": { "custom-provider": provider_with_api_key(&command) }
    }));

    let (_runtime, registry) = harness.registry().await;
    let model = registry
        .find("custom-provider", "test-model")
        .expect("model");
    let auth = registry.get_api_key_and_headers(&model).await;

    let ResolvedRequestAuth::Ok(resolved) = auth else {
        panic!("expected ok");
    };
    assert_eq!(resolved.api_key, Some("stored-key".to_string()));
    assert_eq!(
        std::fs::read_to_string(&counter).expect("counter").trim(),
        "0"
    );
}

#[tokio::test]
async fn get_api_key_and_headers_preserves_the_legacy_missing_key_auth_header_error() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "custom-provider": {
            "baseUrl": "https://example.test/v1",
            "api": "openai-completions",
            "authHeader": true,
            "models": [{ "id": "test-model" }]
        }}
    }));

    let (_runtime, registry) = harness.registry().await;
    let model = registry
        .find("custom-provider", "test-model")
        .expect("model");
    let auth = registry.get_api_key_and_headers(&model).await;

    assert_eq!(
        auth,
        ResolvedRequestAuth::Err {
            error: "No API key found for \"custom-provider\"".to_string(),
        }
    );
}

#[tokio::test]
async fn get_api_key_and_headers_returns_an_error_for_failed_auth_header_resolution() {
    let harness = Harness::new();
    let mut config = provider_with_api_key("!exit 1");
    config["authHeader"] = json!(true);
    harness.write_raw(json!({ "providers": { "custom-provider": config } }));

    let (_runtime, registry) = harness.registry().await;
    let model = registry
        .find("custom-provider", "test-model")
        .expect("model");
    let auth = registry.get_api_key_and_headers(&model).await;

    assert!(!auth.is_ok());
    assert!(
        auth.error()
            .expect("error")
            .contains("Failed to resolve API key for provider \"custom-provider\"")
    );
}

// `Api` 仅用于类型断言（与 TS 的 `Model<Api>` 对应）。
fn _api_is_string(_: &Api) {}

// `ModelCost` 仅用于类型断言（构造 chart 模型时使用）。
fn _cost_is_default() -> ModelCost {
    ModelCost::default()
}

// `ThinkingLevelMap` 仅用于类型断言。
fn _thinking_map() -> ThinkingLevelMap {
    ThinkingLevelMap::new()
}

// 明确使用 `ProviderHeaders`（`headers` 的返回类型）。
fn _headers_alias(value: ProviderHeaders) -> ProviderHeaders {
    value
}

// 明确使用 `InMemoryCodingAgentModelsStore`（`Harness` 的 store 字段）。
fn _store_alias(store: Arc<InMemoryCodingAgentModelsStore>) -> Arc<InMemoryCodingAgentModelsStore> {
    store
}

// 明确使用 `ModelRuntime`（`Harness::create` 的返回类型）。
fn _runtime_alias(runtime: Arc<ModelRuntime>) -> Arc<ModelRuntime> {
    runtime
}
