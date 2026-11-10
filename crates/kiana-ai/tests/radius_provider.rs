//! `packages/ai/test/radius-provider.test.ts` 的 Rust 移植。
//!
//! TS mock 全局 `fetch`；Rust 的 `radius_provider` 用 `default_oauth_fetch`（reqwest）访问
//! `<gateway>/v1/config`，因此刷新相关用例把 provider 的 `gateway` 指向 wiremock 本地
//! 服务器（此时不加载默认网关的静态基线，替换语义与 TS 相同）。

use std::sync::Arc;

use kiana_ai::auth::credential_store::InMemoryCredentialStore;
use kiana_ai::auth::types::{ApiKeyCredential, Credential, CredentialModifyFn, CredentialStore};
use kiana_ai::models::{
    CreateModelsOptions, InMemoryModelsStore, ModelsStore, Provider, create_models,
};
use kiana_ai::providers::all::{RadiusProviderOptions, radius_provider};
use kiana_ai::providers::radius_config::{RadiusGatewayConfig, get_radius_models_from_config};
use kiana_ai::types::AnyModel;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn radius_config() -> serde_json::Value {
    json!({
        "baseUrl": "https://radius.example/v1",
        "models": [
            {
                "id": "balanced",
                "name": "Fresh Balanced",
                "reasoning": true,
                "input": ["text"],
                "cost": { "input": 1, "output": 2, "cacheRead": 0.1, "cacheWrite": 0 },
                "contextWindow": 424242,
                "maxTokens": 32000,
            },
            {
                "id": "organization-only",
                "name": "Organization Only",
                "reasoning": false,
                "input": ["text"],
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                "contextWindow": 128000,
                "maxTokens": 16000,
            },
        ],
    })
}

async fn credential_store() -> Arc<InMemoryCredentialStore> {
    let credentials = InMemoryCredentialStore::new();
    let modify: CredentialModifyFn = Box::new(|_| {
        Box::pin(async {
            Ok(Some(Credential::ApiKey(ApiKeyCredential {
                key: Some("radius-key".into()),
                ..Default::default()
            })))
        })
    });
    credentials
        .modify("radius", modify, Default::default())
        .await
        .expect("store radius credential");
    Arc::new(credentials)
}

fn custom_gateway(server: &MockServer) -> Arc<dyn Provider> {
    radius_provider(RadiusProviderOptions {
        gateway: Some(server.uri()),
        ..Default::default()
    })
}

#[test]
fn ships_a_static_public_catalog_for_the_default_gateway() {
    let provider = radius_provider(RadiusProviderOptions::default());
    let models = provider.get_models();
    assert!(!models.is_empty());
    let balanced = models
        .iter()
        .find(|model| model.id == "balanced")
        .expect("balanced model in public catalog");
    assert_eq!(balanced.provider, "radius");
    assert_eq!(balanced.api, "kiana-messages");
}

#[test]
fn does_not_apply_the_public_radius_catalog_to_custom_gateways() {
    let provider = radius_provider(RadiusProviderOptions {
        id: Some("radius-dev".into()),
        gateway: Some("http://localhost:8788".into()),
        ..Default::default()
    });
    assert_eq!(provider.get_models(), Vec::new());
}

#[tokio::test]
async fn replaces_the_static_public_catalog_with_refreshed_models() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/config"))
        .respond_with(ResponseTemplate::new(200).set_body_json(radius_config()))
        .mount(&server)
        .await;

    let models = create_models(CreateModelsOptions {
        credentials: Some(credential_store().await),
        ..Default::default()
    });
    models.set_provider(custom_gateway(&server));

    let result = models
        .refresh(kiana_ai::models::ModelsRefreshOptions {
            providers: Some(vec!["radius".into()]),
            ..Default::default()
        })
        .await
        .expect("refresh");
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let balanced = models.get_model("radius", "balanced").expect("balanced");
    assert_eq!(balanced.name, "Fresh Balanced");
    assert_eq!(balanced.base_url, "https://radius.example/v1");
    assert_eq!(balanced.context_window, Some(424_242));
    assert!(models.get_model("radius", "organization-only").is_some());
    // Models disabled by the organization must not reappear from the shipped catalog.
    let ids: Vec<String> = models
        .get_models(Some("radius"))
        .into_iter()
        .map(|model| model.id)
        .collect();
    assert_eq!(ids, vec!["balanced", "organization-only"]);
}

#[tokio::test]
async fn replaces_the_static_public_catalog_with_a_cached_catalog_without_network_access() {
    let config: RadiusGatewayConfig =
        serde_json::from_value(radius_config()).expect("radius config");
    let store = Arc::new(InMemoryModelsStore::new());
    store
        .write(
            "radius",
            kiana_ai::models::ModelsStoreEntry {
                models: get_radius_models_from_config("radius", &config)
                    .into_iter()
                    .map(AnyModel::Chat)
                    .collect(),
                checked_at: Some(0),
                ..Default::default()
            },
            Default::default(),
        )
        .await
        .expect("write cached catalog");

    let models = create_models(CreateModelsOptions {
        models_store: Some(store),
        ..Default::default()
    });
    models.set_provider(radius_provider(RadiusProviderOptions::default()));

    models
        .refresh(kiana_ai::models::ModelsRefreshOptions {
            providers: Some(vec!["radius".into()]),
            allow_network: Some(false),
            ..Default::default()
        })
        .await
        .expect("offline refresh");

    assert_eq!(
        models
            .get_model("radius", "balanced")
            .expect("balanced")
            .name,
        "Fresh Balanced"
    );
    assert!(models.get_model("radius", "organization-only").is_some());
    let ids: Vec<String> = models
        .get_models(Some("radius"))
        .into_iter()
        .map(|model| model.id)
        .collect();
    assert_eq!(ids, vec!["balanced", "organization-only"]);
}

#[tokio::test]
async fn exposes_no_models_when_the_organization_disabled_all_of_them() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/config"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "baseUrl": "https://radius.example/v1",
            "models": [],
        })))
        .mount(&server)
        .await;

    let models = create_models(CreateModelsOptions {
        credentials: Some(credential_store().await),
        ..Default::default()
    });
    models.set_provider(custom_gateway(&server));

    let result = models
        .refresh(kiana_ai::models::ModelsRefreshOptions {
            providers: Some(vec!["radius".into()]),
            ..Default::default()
        })
        .await
        .expect("refresh");
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(models.get_models(Some("radius")), Vec::new());
}
