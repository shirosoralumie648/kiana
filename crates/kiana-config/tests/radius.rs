//! `radius.test.ts` 的移植（6 个用例）。
//!
//! 网络侧统一指向本地 `wiremock`（测试禁止外部网络）：默认 Radius 网关用
//! `models.json` 的自定义 `radius-dev` + 本地 `baseUrl` 表达，`create()` 关闭初始网络刷新后
//! 用 provider 作用域的 `refresh({ allowNetwork: true, providers: [id] })` 触发目录抓取。

#[path = "fixtures/model_runtime/mod.rs"]
mod support;

use std::sync::Arc;

use kiana_ai::auth::types::CredentialStore;
use kiana_ai::models::{ModelsRefreshOptions, ModelsStore, ModelsStoreOperationOptions};
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use serde_json::json;
use support::*;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn radius_config(base_url: &str) -> serde_json::Value {
    json!({
        "baseUrl": base_url,
        "models": [{
            "id": "auto",
            "name": "Radius Auto",
            "reasoning": false,
            "input": ["text"],
            "cost": { "input": 1, "output": 2, "cacheRead": 0.1, "cacheWrite": 0.2 },
            "contextWindow": 128000,
            "maxTokens": 16384
        }]
    })
}

fn radius_oauth(extra: Option<serde_json::Value>) -> serde_json::Value {
    let mut credential = json!({
        "type": "oauth",
        "access": "access-token",
        "refresh": "refresh-token",
        "expires": now_ms() + 60 * 60 * 1000
    });
    if let Some(extra) = extra
        && let (Some(object), Some(extra)) = (credential.as_object_mut(), extra.as_object())
    {
        for (key, value) in extra {
            object.insert(key.clone(), value.clone());
        }
    }
    credential
}

/// 创建关闭初始网络刷新的 runtime（radius 用例只抓取显式指定的 provider）。
async fn create_runtime(
    harness: &Harness,
    allow_model_network: bool,
    catalog_base_url: Option<String>,
) -> Arc<ModelRuntime> {
    Arc::new(
        ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(harness.auth_storage.clone() as Arc<dyn CredentialStore>),
            models_path: ModelsPathOption::Path(harness.models_path_string()),
            models_store: Some(harness.models_store.clone()),
            allow_model_network,
            catalog_base_url,
            refresh_on_create: Some(false),
            ..Default::default()
        })
        .await
        .expect("runtime"),
    )
}

/// provider 作用域的联网刷新。
async fn network_refresh(runtime: &ModelRuntime, provider_id: &str) {
    runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(true),
            providers: Some(vec![provider_id.to_string()]),
            force: Some(true),
            ..Default::default()
        })
        .await
        .expect("refresh");
}

#[tokio::test]
async fn restores_the_legacy_credential_catalog_without_network_access() {
    let gateway = "https://radius.example.com/v1";
    let harness = Harness::with_auth(json!({
        "radius": radius_oauth(Some(json!({ "gatewayConfig": radius_config(gateway) })))
    }));
    let runtime = create_runtime(&harness, false, None).await;
    // 初始（离线）刷新由 refreshOnCreate 关闭，用例显式做一次本地刷新。
    runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        })
        .await
        .expect("local refresh");

    let model = runtime.get_model("radius", "auto").expect("radius model");
    assert_eq!(model.api, "kiana-messages");
    assert_eq!(model.base_url, gateway);
    assert_eq!(
        runtime.get_provider("radius").expect("provider").name(),
        "Radius"
    );
    assert!(runtime.has_configured_auth("radius"));
}

#[tokio::test]
async fn fetches_and_stores_the_catalog_for_configured_radius_auth() {
    let server = MockServer::start().await;
    let config_url = format!("{}/v1", server.uri());
    Mock::given(method("GET"))
        .and(path("/v1/config"))
        .and(header("authorization", "Bearer access-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(radius_config(&config_url)))
        .mount(&server)
        .await;

    let harness = Harness::with_auth(json!({
        "radius-dev": radius_oauth(None)
    }));
    harness.write_providers(json!({
        "radius-dev": { "baseUrl": server.uri(), "oauth": "radius" }
    }));
    let runtime = create_runtime(&harness, true, Some(server.uri())).await;
    network_refresh(&runtime, "radius-dev").await;

    assert!(runtime.get_model("radius-dev", "auto").is_some());
    let stored = harness
        .models_store
        .read("radius-dev", ModelsStoreOperationOptions::default())
        .await
        .expect("read")
        .expect("stored catalog");
    assert_eq!(stored.models.len(), 1);

    let requests = server.received_requests().await.unwrap_or_default();
    let config_request = requests
        .iter()
        .find(|request| request.url.path() == "/v1/config")
        .expect("radius config request");
    assert_eq!(
        config_request
            .headers
            .get("authorization")
            .map(|value| value.to_str().unwrap()),
        Some("Bearer access-token")
    );
}

#[tokio::test]
async fn does_not_refresh_catalogs_over_the_network_by_default() {
    let server = MockServer::start().await;
    let gateway = format!("{}/v1", server.uri());
    let harness = Harness::with_auth(json!({
        "radius-dev": radius_oauth(Some(json!({ "gatewayConfig": radius_config(&gateway) })))
    }));
    harness.write_providers(json!({
        "radius-dev": { "baseUrl": server.uri(), "oauth": "radius" }
    }));
    let runtime = create_runtime(&harness, false, Some(server.uri())).await;
    runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        })
        .await
        .expect("local refresh");

    assert!(runtime.get_model("radius-dev", "auto").is_some());
    assert_eq!(
        server.received_requests().await.unwrap_or_default().len(),
        0
    );
}

#[tokio::test]
async fn does_not_fetch_or_make_radius_models_available_without_configured_auth() {
    let server = MockServer::start().await;
    let harness = Harness::new();
    harness.write_providers(json!({
        "radius-dev": { "baseUrl": server.uri(), "oauth": "radius" }
    }));
    let runtime = create_runtime(&harness, true, Some(server.uri())).await;
    network_refresh(&runtime, "radius-dev").await;

    assert!(
        !runtime
            .get_available_snapshot()
            .iter()
            .any(|model| model.provider == "radius-dev")
    );
    let requests = server.received_requests().await.unwrap_or_default();
    assert!(
        !requests
            .iter()
            .any(|request| request.url.path() == "/v1/config")
    );
}

#[tokio::test]
async fn supports_custom_radius_gateways_from_models_json() {
    let server = MockServer::start().await;
    let config_url = format!("{}/v1", server.uri());
    Mock::given(method("GET"))
        .and(path("/v1/config"))
        .respond_with(ResponseTemplate::new(200).set_body_json(radius_config(&config_url)))
        .mount(&server)
        .await;

    let harness = Harness::with_auth(json!({
        "radius-dev": radius_oauth(None)
    }));
    harness.write_providers(json!({
        "radius-dev": {
            "name": "Radius (dev)",
            "baseUrl": server.uri(),
            "oauth": "radius"
        }
    }));
    let runtime = create_runtime(&harness, true, Some(server.uri())).await;
    network_refresh(&runtime, "radius-dev").await;

    let model = runtime
        .get_model("radius-dev", "auto")
        .expect("radius-dev model");
    assert_eq!(model.api, "kiana-messages");
    assert_eq!(model.base_url, config_url);
    assert_eq!(
        runtime.get_provider("radius-dev").expect("provider").name(),
        "Radius (dev)"
    );
}

#[tokio::test]
async fn requires_base_url_for_custom_radius_gateways() {
    let harness = Harness::new();
    harness.write_providers(json!({
        "radius-dev": { "oauth": "radius" }
    }));
    let runtime = create_runtime(&harness, false, None).await;

    assert!(
        runtime
            .get_error()
            .expect("error")
            .contains("\"baseUrl\" is required when \"oauth\" is set")
    );
}
