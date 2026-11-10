//! `packages/ai/test/openrouter-user-models.test.ts` 的 Rust 移植。
//!
//! TS 用 `vi.spyOn(globalThis, "fetch")`；Rust 的 `personalizeOpenRouterCatalog` 走
//! `default_oauth_fetch`（reqwest），因此用 wiremock 本地服务器并把 `context.baseUrl`
//! 指向它（TS 的 `refreshContext()` 不设 `baseUrl`，由默认常量兜底）。

use std::sync::Arc;

use futures::future::BoxFuture;
use kiana_ai::auth::types::{ApiKeyCredential, Credential};
use kiana_ai::error::AiError;
use kiana_ai::models::{ModelsPublication, PublishFn, RefreshModelsContext};
use kiana_ai::providers::openrouter::personalize_open_router_catalog;
use kiana_ai::types::{AnyModel, Model, ModelCost, ModelInputModality, ModelType};
use kiana_ai::utils::abort::AbortScope;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn zero_cost() -> ModelCost {
    ModelCost::default()
}

fn chat_model(id: &str) -> Model {
    Model {
        id: id.into(),
        name: id.into(),
        api: "openai-completions".into(),
        provider: "openrouter".into(),
        base_url: "https://openrouter.ai/api/v1".into(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text],
        cost: ModelCost {
            input: 1.0,
            output: 2.0,
            cache_read: 0.1,
            cache_write: 0.0,
            tiers: None,
        },
        context_window: Some(200_000),
        max_tokens: Some(50_000),
        ..Default::default()
    }
}

fn image_model() -> AnyModel {
    let mut model = Model {
        id: "image".into(),
        name: "image".into(),
        api: "openrouter-images".into(),
        provider: "openrouter".into(),
        base_url: "https://openrouter.ai/api/v1".into(),
        input: vec![ModelInputModality::Text],
        output: Some(vec!["image".into()]),
        cost: zero_cost(),
        ..Default::default()
    };
    model.model_type = Some(ModelType::Image);
    AnyModel::Image(model)
}

fn classifier_model() -> AnyModel {
    let mut model = Model {
        id: "classifier".into(),
        name: "classifier".into(),
        api: "typesafe-system-one".into(),
        provider: "openrouter".into(),
        base_url: "https://openrouter.ai/api/v1".into(),
        input: vec![ModelInputModality::Text],
        cost: zero_cost(),
        context_window: Some(64_000),
        ..Default::default()
    };
    model.model_type = Some(ModelType::Classifier);
    AnyModel::Classifier(model)
}

fn publish_stub() -> PublishFn {
    Arc::new(|_publication: ModelsPublication| {
        let future: BoxFuture<'static, Result<bool, AiError>> = Box::pin(async { Ok(true) });
        future
    })
}

fn refresh_context(base_url: String) -> RefreshModelsContext {
    RefreshModelsContext {
        credential: Some(Credential::ApiKey(ApiKeyCredential {
            key: Some("sk-or-test".into()),
            ..Default::default()
        })),
        stored: None,
        publish: publish_stub(),
        allow_network: true,
        force: None,
        signal: AbortScope::new(),
        base_url: Some(base_url),
    }
}

fn model_ids(models: &[AnyModel]) -> Vec<&str> {
    models
        .iter()
        .map(|model| model.model().id.as_str())
        .collect()
}

#[tokio::test]
async fn keeps_only_the_keys_chat_models_with_the_keys_limits_and_prices() {
    let server = MockServer::start().await;
    let data = json!({
        "data": [
            {
                "id": "listed",
                "context_length": 1_000_000,
                "top_provider": { "context_length": 262_144, "max_completion_tokens": 32_768 },
                "pricing": { "prompt": "0.000003", "completion": "0.000015" },
            },
            // A missing limit keeps the catalog value, so generator fixes for missing OpenRouter data survive.
            { "id": "no-limits", "context_length": 0, "top_provider": { "context_length": null, "max_completion_tokens": null } },
            // Router models report -1, meaning the price depends on the model they pick.
            { "id": "router", "pricing": { "prompt": "-1", "completion": "-1" } },
        ],
    });
    Mock::given(method("GET"))
        .and(path("/models/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(data))
        .mount(&server)
        .await;

    let catalog = vec![
        AnyModel::Chat(chat_model("listed")),
        AnyModel::Chat(chat_model("no-limits")),
        AnyModel::Chat(chat_model("router")),
        AnyModel::Chat(chat_model("blocked")),
        image_model(),
        classifier_model(),
    ];

    let result = personalize_open_router_catalog(catalog, refresh_context(server.uri()))
        .await
        .expect("personalized catalog");

    assert_eq!(
        model_ids(&result),
        vec!["listed", "no-limits", "router", "image", "classifier"]
    );
    let listed = result[0].model();
    assert_eq!(listed.context_window, Some(262_144));
    assert_eq!(listed.max_tokens, Some(32_768));
    assert_eq!(listed.cost.input, 3.0);
    assert_eq!(listed.cost.output, 15.0);
    assert_eq!(listed.cost.cache_read, 0.0);
    assert_eq!(listed.cost.cache_write, 0.0);

    let no_limits = result[1].model();
    assert_eq!(no_limits.context_window, Some(200_000));
    assert_eq!(no_limits.max_tokens, Some(50_000));
    assert_eq!(no_limits.cost, chat_model("x").cost);

    assert_eq!(result[2].model().cost, zero_cost());
    // `/models/user` lists text models only, so image and classifier models are not filtered.
    assert_eq!(result[3].model_type(), ModelType::Image);
    assert_eq!(result[4].model_type(), ModelType::Classifier);
}

#[tokio::test]
async fn returns_the_catalog_unchanged_when_the_endpoint_rejects_the_key_or_is_missing() {
    for status in [401u16, 403, 404] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models/user"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;

        let catalog = vec![AnyModel::Chat(chat_model("a"))];
        let result = personalize_open_router_catalog(catalog, refresh_context(server.uri()))
            .await
            .expect("unchanged catalog");
        assert_eq!(model_ids(&result), vec!["a"], "status {status}");
    }
}

/// TS `cost: entry.pricing ? openRouterCost(entry.pricing) : model.cost`：只有真值 pricing 才
/// 替换成本；`null` / `0` / `""` / `false` 等假值保留目录成本（对象即使为空也算真值）。
#[tokio::test]
async fn keeps_the_catalog_cost_for_falsy_pricing_values() {
    let server = MockServer::start().await;
    let data = json!({
        "data": [
            { "id": "null-price", "pricing": null },
            { "id": "zero-price", "pricing": 0 },
            { "id": "empty-price", "pricing": "" },
            { "id": "false-price", "pricing": false },
            { "id": "object-price", "pricing": { "prompt": "0.000003", "completion": "0.000015" } },
        ],
    });
    Mock::given(method("GET"))
        .and(path("/models/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(data))
        .mount(&server)
        .await;

    let ids = [
        "null-price",
        "zero-price",
        "empty-price",
        "false-price",
        "object-price",
    ];
    let catalog = ids
        .iter()
        .map(|id| AnyModel::Chat(chat_model(id)))
        .collect();
    let result = personalize_open_router_catalog(catalog, refresh_context(server.uri()))
        .await
        .expect("personalized catalog");

    let catalog_cost = chat_model("x").cost;
    for model in &result[..4] {
        assert_eq!(model.model().cost, catalog_cost, "{}", model.model().id);
    }
    assert_eq!(result[4].model().cost.input, 3.0);
    assert_eq!(result[4].model().cost.output, 15.0);
}
