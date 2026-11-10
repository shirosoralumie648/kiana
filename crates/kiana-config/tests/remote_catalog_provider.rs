//! `remote-catalog-provider.test.ts`（10 个用例）的 1:1 移植，HTTP 层用 wiremock。
//!
//! 覆盖：keyed catalog、版本头与刷新 TTL、image/classifier 覆盖与未知类型丢弃、
//! 本地/远端目录新旧选择、etag 304、404/501 与瞬时失败、并发刷新的代际保护，
//! 以及个性化目录的保存/回退语义。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kiana_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, AuthError, AuthResult, Credential, ProviderAuth,
};
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::models::{
    CreateModelsOptions, CreateProviderOptions, InMemoryModelsStore, ModelsPublicationPersist,
    ModelsStore, ModelsStoreOperationOptions, Provider, ProviderApiImpl, ProviderStreams,
    PublishFn, RefreshModelsContext, create_models, create_provider,
};
use kiana_ai::types::{
    AnyModel, Model, ModelCost, ModelInputModality, ModelType, SimpleStreamOptions, StreamOptions,
    TranscriptContext,
};
use kiana_ai::utils::abort::AbortScope;
use kiana_config::models::catalog_http::{parse_catalog, request_catalog};
use kiana_config::models::remote_catalog::{
    PersonalizeCatalog, REMOTE_CATALOG_MODEL_TYPES, REMOTE_CATALOG_REFRESH_INTERVAL_MS,
    merge_models, with_personalized_catalog, with_remote_catalog,
};
use kiana_core::config::VERSION;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PROVIDER_ID: &str = "test-provider";

struct TestStreams;

impl ProviderStreams for TestStreams {
    fn stream(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: StreamOptions,
    ) -> AssistantMessageEventStream {
        panic!("not used")
    }

    fn stream_simple(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        panic!("not used")
    }
}

struct TestAuth;

#[async_trait]
impl ApiKeyAuth for TestAuth {
    fn name(&self) -> &str {
        "Test"
    }

    async fn resolve(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        Ok(Some(AuthResult {
            source: Some("test".to_string()),
            ..AuthResult::default()
        }))
    }
}

fn model_value(id: &str) -> Value {
    json!({
        "id": id,
        "name": id,
        "api": "openai-completions",
        "provider": PROVIDER_ID,
        "baseUrl": "https://example.test/v1",
        "reasoning": false,
        "input": ["text"],
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
        "contextWindow": 1000,
        "maxTokens": 100
    })
}

fn model(id: &str) -> Model {
    Model {
        id: id.to_string(),
        name: id.to_string(),
        api: "openai-completions".to_string(),
        provider: PROVIDER_ID.to_string(),
        base_url: "https://example.test/v1".to_string(),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(1000),
        max_tokens: Some(100),
        ..Model::default()
    }
}

fn builtin_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: PROVIDER_ID.to_string(),
        auth: ProviderAuth {
            api_key: Some(Arc::new(TestAuth)),
            oauth: None,
        },
        models: vec![AnyModel::Chat(model("static"))],
        api: Some(ProviderApiImpl::Single(Arc::new(TestStreams))),
        ..CreateProviderOptions::default()
    })
    .expect("valid provider")
}

fn test_provider(server: &MockServer, local_generated_at: Option<i64>) -> Arc<dyn Provider> {
    with_remote_catalog(
        builtin_provider(),
        Some(server.uri().as_str()),
        local_generated_at,
    )
}

fn publish(store: Arc<InMemoryModelsStore>) -> PublishFn {
    Arc::new(move |publication| {
        let store = Arc::clone(&store);
        Box::pin(async move {
            match publication.persist {
                ModelsPublicationPersist::Unchanged => {}
                ModelsPublicationPersist::Delete => {
                    store
                        .delete(PROVIDER_ID, ModelsStoreOperationOptions::default())
                        .await?;
                }
                ModelsPublicationPersist::Entry(entry) => {
                    store
                        .write(PROVIDER_ID, entry, ModelsStoreOperationOptions::default())
                        .await?;
                }
            }
            if let Some(update) = publication.update {
                update();
            }
            Ok(true)
        })
    })
}

#[derive(Default)]
struct RefreshOverrides {
    allow_network: bool,
    force: Option<bool>,
    signal: Option<AbortScope>,
}

#[allow(clippy::result_large_err)]
async fn refresh_provider(
    provider: &Arc<dyn Provider>,
    store: &Arc<InMemoryModelsStore>,
    overrides: RefreshOverrides,
) -> Result<(), kiana_ai::AiError> {
    let stored = store
        .read(PROVIDER_ID, ModelsStoreOperationOptions::default())
        .await
        .expect("read store");
    provider
        .refresh_models(RefreshModelsContext {
            credential: Some(Credential::ApiKey(
                kiana_ai::auth::types::ApiKeyCredential {
                    key: None,
                    env: None,
                    extra: Default::default(),
                },
            )),
            stored,
            publish: publish(Arc::clone(store)),
            allow_network: overrides.allow_network,
            force: overrides.force,
            signal: overrides.signal.unwrap_or_default(),
            base_url: None,
        })
        .await
}

async fn read_store(
    store: &Arc<InMemoryModelsStore>,
) -> Option<kiana_ai::models::ModelsStoreEntry> {
    store
        .read(PROVIDER_ID, ModelsStoreOperationOptions::default())
        .await
        .expect("read")
}

fn model_ids(models: Vec<Model>) -> Vec<String> {
    models.into_iter().map(|model| model.id).collect()
}

fn any_model_ids(models: Vec<AnyModel>) -> Vec<String> {
    models
        .into_iter()
        .map(|model| model.model().id.clone())
        .collect()
}

fn entry_ids(entry: &kiana_ai::models::ModelsStoreEntry) -> Vec<String> {
    entry
        .models
        .iter()
        .map(|model| model.model().id.clone())
        .collect()
}

fn store_models(store: &Arc<InMemoryModelsStore>) -> Arc<dyn ModelsStore> {
    Arc::clone(store) as Arc<dyn ModelsStore>
}

fn default_allow_network() -> RefreshOverrides {
    RefreshOverrides {
        allow_network: true,
        force: None,
        signal: None,
    }
}

fn rfc2822(millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(millis)
        .expect("valid timestamp")
        .to_rfc2822()
}

// ---------------------------------------------------------------------------
// remote catalog
// ---------------------------------------------------------------------------

#[tokio::test]
async fn without_a_catalog_base_url_refresh_stays_local() {
    let provider = with_remote_catalog(builtin_provider(), None, None);
    let store = Arc::new(InMemoryModelsStore::new());
    refresh_provider(&provider, &store, default_allow_network())
        .await
        .expect("no network request is attempted");

    assert_eq!(model_ids(provider.get_models()), ["static"]);
    assert!(read_store(&store).await.is_none());
}

#[tokio::test]
async fn parses_keyed_catalogs_sends_version_headers_and_observes_the_ttl() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/models/providers/test-provider"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"dynamic": model_value("dynamic")}))
                .insert_header("content-type", "application/json"),
        )
        .mount(&server)
        .await;

    let provider = test_provider(&server, None);
    let store = Arc::new(InMemoryModelsStore::new());
    refresh_provider(&provider, &store, default_allow_network())
        .await
        .expect("first refresh");
    refresh_provider(&provider, &store, default_allow_network())
        .await
        .expect("second refresh (TTL)");
    refresh_provider(
        &provider,
        &store,
        RefreshOverrides {
            force: Some(true),
            ..default_allow_network()
        },
    )
    .await
    .expect("forced refresh");

    assert_eq!(model_ids(provider.get_models()), ["static", "dynamic"]);
    let stored = read_store(&store).await.expect("stored");
    assert_eq!(entry_ids(&stored), ["dynamic"]);

    let requests = server.received_requests().await.expect("recorded");
    assert_eq!(requests.len(), 2);
    let user_agent = requests[0]
        .headers
        .get("user-agent")
        .expect("user agent")
        .to_str()
        .expect("ascii");
    assert!(
        user_agent.contains(&format!("kiana/{VERSION}")),
        "unexpected user agent {user_agent}"
    );
    assert_eq!(
        requests[0].url.path(),
        "/api/models/providers/test-provider"
    );
    let types = requests[0]
        .url
        .query_pairs()
        .find(|(name, _)| name == "types")
        .map(|(_, value)| value.into_owned());
    assert_eq!(types.as_deref(), Some("chat,image,classifier"));
}

#[tokio::test]
async fn overlays_image_and_classifier_models_and_drops_unknown_types() {
    let server = MockServer::start().await;
    let mut chat = model_value("chat");
    chat["type"] = json!("chat");
    let flux = json!({
        "type": "image",
        "id": "flux",
        "name": "FLUX",
        "api": "openrouter-images",
        "provider": PROVIDER_ID,
        "baseUrl": "https://example.test/v1",
        "input": ["text"],
        "output": ["image"],
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}
    });
    let jev = json!({
        "type": "classifier",
        "id": "jev",
        "name": "Jev",
        "api": "typesafe-system-one",
        "provider": PROVIDER_ID,
        "baseUrl": "https://example.test/v1",
        "input": ["text"],
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
        "contextWindow": 64000
    });
    let clip = {
        let mut value = model_value("clip");
        value["type"] = json!("video");
        value
    };
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "chat": chat,
            "flux": flux,
            "jev": jev,
            "clip": clip
        })))
        .mount(&server)
        .await;

    let provider = test_provider(&server, None);
    let store = Arc::new(InMemoryModelsStore::new());
    refresh_provider(&provider, &store, default_allow_network())
        .await
        .expect("refresh");

    let models = create_models(CreateModelsOptions {
        models_store: Some(store_models(&store)),
        ..CreateModelsOptions::default()
    });
    models.set_provider(Arc::clone(&provider));
    assert_eq!(
        any_model_ids(models.get_all_models(Some(PROVIDER_ID))),
        ["static", "chat", "flux", "jev"]
    );
    assert_eq!(
        models
            .get_model_of_type(ModelType::Image, PROVIDER_ID, "flux")
            .map(|model| model.model_type),
        Some(Some(ModelType::Image))
    );
    assert_eq!(
        models
            .get_model_of_type(ModelType::Classifier, PROVIDER_ID, "jev")
            .map(|model| model.model_type),
        Some(Some(ModelType::Classifier))
    );
    assert!(models.get_model(PROVIDER_ID, "flux").is_none());
    let stored = read_store(&store).await.expect("stored");
    assert_eq!(entry_ids(&stored), ["chat", "flux", "jev"]);
}

#[tokio::test]
async fn prefers_the_newer_of_generated_and_remote_catalogs() {
    let local_generated_at = chrono::DateTime::parse_from_rfc3339("2026-07-23T10:00:00.000Z")
        .unwrap()
        .timestamp_millis();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"old": model_value("old")}))
                .insert_header("last-modified", rfc2822(local_generated_at - 60_000)),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let newer_header = rfc2822(local_generated_at + 60_000);
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"newer": model_value("newer")}))
                .insert_header("last-modified", newer_header.clone()),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let provider = test_provider(&server, Some(local_generated_at));
    let store = Arc::new(InMemoryModelsStore::new());
    refresh_provider(&provider, &store, default_allow_network())
        .await
        .expect("first refresh");
    assert_eq!(model_ids(provider.get_models()), ["static"]);

    refresh_provider(
        &provider,
        &store,
        RefreshOverrides {
            force: Some(true),
            ..default_allow_network()
        },
    )
    .await
    .expect("second refresh");
    assert_eq!(model_ids(provider.get_models()), ["static", "newer"]);
    let stored = read_store(&store).await.expect("stored");
    assert_eq!(
        stored.last_modified,
        Some(
            chrono::DateTime::parse_from_rfc2822(&newer_header)
                .unwrap()
                .timestamp_millis()
        )
    );
}

#[tokio::test]
async fn revalidates_with_etag_and_keeps_the_overlay_on_304() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"dynamic": model_value("dynamic")}))
                .insert_header("content-type", "application/json")
                .insert_header("etag", "\"catalog-1\""),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(304).insert_header("etag", "\"catalog-1\""))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let provider = test_provider(&server, None);
    let store = Arc::new(InMemoryModelsStore::new());
    refresh_provider(&provider, &store, default_allow_network())
        .await
        .expect("first refresh");
    let requests = server.received_requests().await.expect("recorded");
    assert!(requests[0].headers.get("if-none-match").is_none());
    let stored = read_store(&store).await.expect("stored");
    assert_eq!(stored.etag.as_deref(), Some("\"catalog-1\""));
    let checked_at = stored.checked_at.expect("checkedAt");

    refresh_provider(
        &provider,
        &store,
        RefreshOverrides {
            force: Some(true),
            ..default_allow_network()
        },
    )
    .await
    .expect("second refresh");
    let requests = server.received_requests().await.expect("recorded");
    assert_eq!(
        requests[1]
            .headers
            .get("if-none-match")
            .and_then(|value| value.to_str().ok()),
        Some("\"catalog-1\"")
    );
    assert_eq!(model_ids(provider.get_models()), ["static", "dynamic"]);
    let stored = read_store(&store).await.expect("stored");
    assert_eq!(entry_ids(&stored), ["dynamic"]);
    assert_eq!(stored.etag.as_deref(), Some("\"catalog-1\""));
    assert!(stored.checked_at.expect("checkedAt") >= checked_at);
}

#[tokio::test]
async fn drops_a_stale_etag_when_the_overlay_becomes_unavailable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"dynamic": model_value("dynamic")}))
                .insert_header("etag", "\"catalog-1\""),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(501).set_body_string("not implemented"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let provider = test_provider(&server, None);
    let store = Arc::new(InMemoryModelsStore::new());
    refresh_provider(&provider, &store, default_allow_network())
        .await
        .expect("first refresh");
    refresh_provider(
        &provider,
        &store,
        RefreshOverrides {
            force: Some(true),
            ..default_allow_network()
        },
    )
    .await
    .expect("second refresh");
    let stored = read_store(&store).await.expect("stored");
    assert!(stored.etag.is_none());
    assert_eq!(stored.last_modified, Some(0));
}

#[tokio::test]
async fn keeps_the_etag_and_overlay_after_a_transient_failure() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"dynamic": model_value("dynamic")}))
                .insert_header("etag", "\"catalog-1\""),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429).set_body_string("rate limited"))
        .up_to_n_times(3)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(304).insert_header("etag", "\"catalog-1\""))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let provider = test_provider(&server, None);
    let store = Arc::new(InMemoryModelsStore::new());
    refresh_provider(&provider, &store, default_allow_network())
        .await
        .expect("first refresh");
    let error = refresh_provider(
        &provider,
        &store,
        RefreshOverrides {
            force: Some(true),
            ..default_allow_network()
        },
    )
    .await
    .expect_err("transient failure");
    assert_eq!(
        error.message,
        "Model catalog request failed for test-provider: 429"
    );

    let stored = read_store(&store).await.expect("stored");
    assert_eq!(stored.etag.as_deref(), Some("\"catalog-1\""));
    assert_eq!(entry_ids(&stored), ["dynamic"]);

    refresh_provider(
        &provider,
        &store,
        RefreshOverrides {
            force: Some(true),
            ..default_allow_network()
        },
    )
    .await
    .expect("third refresh");
    let requests = server.received_requests().await.expect("recorded");
    assert_eq!(
        requests[4]
            .headers
            .get("if-none-match")
            .and_then(|value| value.to_str().ok()),
        Some("\"catalog-1\"")
    );
    assert_eq!(model_ids(provider.get_models()), ["static", "dynamic"]);
}

#[tokio::test]
async fn newer_request_bypasses_a_stalled_older_request_without_stale_publication() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(300))
                .set_body_json(json!({"older": model_value("older")})),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"newer": model_value("newer")})),
        )
        .mount(&server)
        .await;

    let provider = test_provider(&server, None);
    let store = Arc::new(InMemoryModelsStore::new());
    let models = create_models(CreateModelsOptions {
        models_store: Some(store_models(&store)),
        ..CreateModelsOptions::default()
    });
    models.set_provider(Arc::clone(&provider));

    let first = {
        let models = Arc::clone(&models);
        tokio::spawn(async move {
            models
                .refresh(kiana_ai::models::ModelsRefreshOptions {
                    providers: Some(vec![PROVIDER_ID.to_string()]),
                    force: Some(true),
                    ..kiana_ai::models::ModelsRefreshOptions::default()
                })
                .await
        })
    };
    // 等第一个请求真正到达服务端（wiremock 在处理前记录）。
    for _ in 0..400 {
        let received = server.received_requests().await.expect("recorded");
        if !received.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    models
        .refresh(kiana_ai::models::ModelsRefreshOptions {
            providers: Some(vec![PROVIDER_ID.to_string()]),
            force: Some(true),
            ..kiana_ai::models::ModelsRefreshOptions::default()
        })
        .await
        .expect("second refresh");
    let _ = first.await.expect("first task");
    assert_eq!(model_ids(provider.get_models()), ["static", "newer"]);

    // 迟到的旧响应不能覆盖新目录。
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(model_ids(provider.get_models()), ["static", "newer"]);
    let stored = read_store(&store).await.expect("stored");
    assert_eq!(entry_ids(&stored), ["newer"]);
}

#[tokio::test]
async fn treats_unimplemented_routes_as_an_unavailable_overlay() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(501).set_body_string("not implemented"))
        .mount(&server)
        .await;

    let provider = test_provider(&server, None);
    let store = Arc::new(InMemoryModelsStore::new());
    refresh_provider(&provider, &store, default_allow_network())
        .await
        .expect("refresh");
    assert_eq!(model_ids(provider.get_models()), ["static"]);
    let stored = read_store(&store).await.expect("stored");
    assert!(stored.models.is_empty());
    assert!(stored.checked_at.is_some());
}

// ---------------------------------------------------------------------------
// personalized catalog
// ---------------------------------------------------------------------------

fn personalized(server: &MockServer, personalize: PersonalizeCatalog) -> Arc<dyn Provider> {
    with_personalized_catalog(
        builtin_provider(),
        personalize,
        Some(server.uri().as_str()),
        None,
    )
}

#[tokio::test]
async fn keeps_the_saved_list_and_saves_nothing_when_personalization_fails() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
        .mount(&server)
        .await;
    let store = Arc::new(InMemoryModelsStore::new());
    let saved = kiana_ai::models::ModelsStoreEntry {
        models: vec![AnyModel::Chat(model("allowed"))],
        checked_at: Some(1),
        ..kiana_ai::models::ModelsStoreEntry::default()
    };
    store
        .write(
            PROVIDER_ID,
            saved.clone(),
            ModelsStoreOperationOptions::default(),
        )
        .await
        .expect("save");
    let personalize: PersonalizeCatalog = Arc::new(|_, _| {
        Box::pin(async {
            Err(kiana_ai::AiError::new(
                kiana_ai::AiErrorKind::Other,
                "user models request failed",
            ))
        })
    });
    let provider = personalized(&server, personalize);

    let error = refresh_provider(&provider, &store, default_allow_network())
        .await
        .expect_err("personalize fails");
    assert!(error.message.contains("user models request failed"));
    assert_eq!(model_ids(provider.get_models()), ["allowed"]);
    assert_eq!(read_store(&store).await, Some(saved));
}

#[tokio::test]
async fn restores_a_saved_list_as_is_and_falls_back_only_when_it_is_empty() {
    let server = MockServer::start().await;
    let personalize: PersonalizeCatalog = Arc::new(|_, _| {
        Box::pin(async {
            Err(kiana_ai::AiError::new(
                kiana_ai::AiErrorKind::Other,
                "not used offline",
            ))
        })
    });
    let store = Arc::new(InMemoryModelsStore::new());
    let provider = personalized(&server, personalize);

    // 旧版本可能在 kiana.dev 目录缺失/失败后保存空列表；只有这种条目能是空。
    store
        .write(
            PROVIDER_ID,
            kiana_ai::models::ModelsStoreEntry {
                models: Vec::new(),
                checked_at: Some(1),
                ..kiana_ai::models::ModelsStoreEntry::default()
            },
            ModelsStoreOperationOptions::default(),
        )
        .await
        .expect("save empty");
    refresh_provider(
        &provider,
        &store,
        RefreshOverrides {
            allow_network: false,
            ..RefreshOverrides::default()
        },
    )
    .await
    .expect("offline refresh");
    assert_eq!(model_ids(provider.get_models()), ["static"]);

    // 该 key 可能不能使用任何 chat 模型：保留 image 模型、隐藏 chat 模型。
    store
        .write(
            PROVIDER_ID,
            kiana_ai::models::ModelsStoreEntry {
                models: vec![AnyModel::Image(Model {
                    model_type: Some(ModelType::Image),
                    api: "openrouter-images".to_string(),
                    output: Some(vec!["image".to_string()]),
                    ..model("image")
                })],
                checked_at: Some(1),
                ..kiana_ai::models::ModelsStoreEntry::default()
            },
            ModelsStoreOperationOptions::default(),
        )
        .await
        .expect("save image");
    refresh_provider(
        &provider,
        &store,
        RefreshOverrides {
            allow_network: false,
            ..RefreshOverrides::default()
        },
    )
    .await
    .expect("offline refresh");
    assert!(provider.get_models().is_empty());
    assert_eq!(any_model_ids(provider.get_all_models()), ["image"]);
}

#[test]
fn constants_match_the_typescript_source() {
    assert_eq!(REMOTE_CATALOG_REFRESH_INTERVAL_MS, 4 * 60 * 60 * 1000);
    assert_eq!(
        REMOTE_CATALOG_MODEL_TYPES,
        [ModelType::Chat, ModelType::Image, ModelType::Classifier]
    );
}

#[test]
fn merge_models_keeps_the_first_position() {
    let mut image = model("shared");
    image.model_type = Some(ModelType::Image);
    let merged = merge_models(
        &[AnyModel::Chat(model("a")), AnyModel::Chat(model("shared"))],
        &[AnyModel::Image(image), AnyModel::Chat(model("a"))],
    );
    let ids: Vec<String> = merged
        .iter()
        .map(|model| model.model().id.clone())
        .collect();
    assert_eq!(ids, ["a", "shared", "shared"]);
    // chat 的 "a" 被后插入的 chat 覆盖（保留首次位置）；image 的 "shared" 与 chat 不同键。
    assert!(matches!(merged[0], AnyModel::Chat(_)));
    assert!(matches!(merged[1], AnyModel::Chat(_)));
    assert!(matches!(merged[2], AnyModel::Image(_)));
    assert_eq!(merged.len(), 3);
}

/// §3.2 的逐字错误：非数组/非对象的目录值。
#[test]
fn parse_catalog_rejects_non_collection_values_with_the_verbatim_error() {
    for value in [json!(null), json!("nope"), json!(42), json!(true)] {
        let error = parse_catalog("demo", &value).expect_err("invalid catalog");
        assert_eq!(error.message, "Invalid model catalog for provider \"demo\"");
    }
}

#[test]
fn parse_catalog_keeps_partial_entries_and_explicit_chat_type() {
    // TS 的 parseCatalog 只检查 object / id / type，条目原样保留。
    let value = json!([
        {"id": "chat", "type": "chat"},
        {"id": "only-id"},
        {"id": "img", "type": "image", "name": "Img"},
        {"type": "image"},
        {"id": "vid", "type": "video"},
        {"id": "null-type", "type": null},
    ]);
    let parsed = parse_catalog(PROVIDER_ID, &value).expect("parsed");
    let ids: Vec<&str> = parsed
        .models
        .iter()
        .map(|model| model.model().id.as_str())
        .collect();
    assert_eq!(ids, ["chat", "only-id", "img"]);
    assert_eq!(parsed.raw_models.len(), 3);
    // provider 强制改写；显式 type:"chat" 保留。
    assert_eq!(parsed.raw_models[0]["provider"], json!(PROVIDER_ID));
    assert_eq!(parsed.raw_models[0]["type"], json!("chat"));
    assert_eq!(
        parsed.models[0].model_type(),
        ModelType::Chat,
        "explicit type:chat stays a chat model"
    );
    assert_eq!(
        parsed.models[0].model().model_type,
        Some(ModelType::Chat),
        "explicit type:chat survives serialization"
    );
    assert_eq!(parsed.models[2].model_type(), ModelType::Image);
}

#[tokio::test]
async fn keeps_partial_catalog_entries_in_the_overlay_and_store() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("etag", "\"audit\"")
                .set_body_json(json!({
                    "partial": {"id": "partial", "type": "chat"},
                    "complete": model_value("complete"),
                })),
        )
        .mount(&server)
        .await;
    let provider = test_provider(&server, None);
    let store = Arc::new(InMemoryModelsStore::new());
    refresh_provider(&provider, &store, default_allow_network())
        .await
        .expect("refresh");
    assert_eq!(
        any_model_ids(provider.get_all_models()),
        ["static", "partial", "complete"]
    );
    let stored = read_store(&store).await.expect("stored");
    assert_eq!(entry_ids(&stored), ["partial", "complete"]);
    assert_eq!(stored.models[0].model().model_type, Some(ModelType::Chat));
    let live = provider.get_all_models();
    let partial = live
        .iter()
        .find(|model| model.model().id == "partial")
        .unwrap();
    assert_eq!(
        serde_json::to_string(partial).unwrap(),
        r#"{"id":"partial","type":"chat","provider":"test-provider"}"#
    );
    let serialized = serde_json::to_string(&stored).unwrap();
    assert!(
        serialized.starts_with(
            r#"{"models":[{"id":"partial","type":"chat","provider":"test-provider"},"#
        ),
        "{serialized}"
    );
    assert!(serialized.contains("lastModified"), "{serialized}");
    let etag = serialized
        .find("\"etag\"")
        .expect("fresh catalog etag absent");
    let checked = serialized.find("\"checkedAt\"").unwrap();
    assert!(etag < checked, "{serialized}");

    // The complete object is raw too: an RPC/wire writer must not apply the
    // models.json modelFromJson layout or drop remote-only fields.
    let mut complete = model_value("complete");
    complete.as_object_mut().unwrap().shift_remove("id");
    complete
        .as_object_mut()
        .unwrap()
        .insert("id".into(), json!("complete"));
    complete["futureCatalogField"] = json!({"preserved": true});
    let wire = kiana_agent_core::wire_keys::to_wire_value(&AnyModel::Remote {
        model: model("complete"),
        json: complete.clone(),
    })
    .unwrap();
    assert_eq!(wire, complete);
    assert_eq!(
        wire.as_object().unwrap().keys().next_back().unwrap(),
        "futureCatalogField"
    );

    // A second provider restores the same sparse object while offline. It must
    // not be dropped because it lacks fields required by the typed projection.
    let restored_provider = test_provider(&server, None);
    refresh_provider(
        &restored_provider,
        &store,
        RefreshOverrides {
            allow_network: false,
            force: None,
            signal: None,
        },
    )
    .await
    .unwrap();
    let restored = restored_provider.get_all_models();
    let partial = restored
        .iter()
        .find(|model| model.model().id == "partial")
        .unwrap();
    assert_eq!(
        serde_json::to_value(partial).unwrap(),
        json!({
            "id": "partial", "type": "chat", "provider": PROVIDER_ID,
        })
    );

    let stale_provider = test_provider(&server, Some(i64::MAX));
    refresh_provider(
        &stale_provider,
        &store,
        RefreshOverrides {
            allow_network: false,
            force: None,
            signal: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(any_model_ids(stale_provider.get_all_models()), ["static"]);
}

#[tokio::test]
async fn empty_etag_is_not_forwarded() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    let signal = AbortScope::new();
    request_catalog(PROVIDER_ID, &server.uri(), &signal, Some(""))
        .await
        .expect("request");
    let requests = server.received_requests().await.expect("recorded");
    assert!(requests[0].headers.get("if-none-match").is_none());
    request_catalog(PROVIDER_ID, &server.uri(), &signal, Some("\"x\""))
        .await
        .expect("request");
    let requests = server.received_requests().await.expect("recorded");
    assert_eq!(
        requests[1]
            .headers
            .get("if-none-match")
            .and_then(|value| value.to_str().ok()),
        Some("\"x\"")
    );
}

#[tokio::test]
async fn parses_date_only_and_zoneless_last_modified() {
    use chrono::TimeZone;

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"a": model_value("a")}))
                .insert_header("last-modified", "2026-07-23"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"b": model_value("b")}))
                .insert_header("last-modified", "2026-07-23T10:00:00"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let provider = test_provider(&server, None);
    let store = Arc::new(InMemoryModelsStore::new());
    refresh_provider(&provider, &store, default_allow_network())
        .await
        .expect("first refresh");
    let stored = read_store(&store).await.expect("stored");
    let utc_midnight = chrono::NaiveDate::from_ymd_opt(2026, 7, 23)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis();
    assert_eq!(stored.last_modified, Some(utc_midnight));

    refresh_provider(
        &provider,
        &store,
        RefreshOverrides {
            force: Some(true),
            ..default_allow_network()
        },
    )
    .await
    .expect("second refresh");
    let stored = read_store(&store).await.expect("stored");
    let local = chrono::Local
        .from_local_datetime(
            &chrono::NaiveDate::from_ymd_opt(2026, 7, 23)
                .unwrap()
                .and_hms_opt(10, 0, 0)
                .unwrap(),
        )
        .single()
        .expect("unambiguous local time")
        .timestamp_millis();
    assert_eq!(stored.last_modified, Some(local));
}

/// 回归：`Last-Modified` 的 `UTC` 后缀与带冒号的数字偏移是 V8 `Date.parse` 接受的形式；
/// 解析失败会退回 `lastModified = 0`，目录被判定为过旧而静默丢弃（TS 会应用它）。
#[tokio::test]
async fn applies_catalog_with_utc_and_colon_offset_last_modified() {
    let expected = 1_445_412_480_000; // 2015-10-21T07:28:00Z
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"a": model_value("a")}))
                .insert_header("last-modified", "Wed, 21 Oct 2015 07:28:00 UTC"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"b": model_value("b")}))
                .insert_header("last-modified", "Wed, 21 Oct 2015 07:28:00 +00:00"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let provider = test_provider(&server, Some(expected - 1));
    let store = Arc::new(InMemoryModelsStore::new());
    refresh_provider(&provider, &store, default_allow_network())
        .await
        .expect("first refresh");
    let stored = read_store(&store).await.expect("stored");
    assert_eq!(stored.last_modified, Some(expected));
    assert_eq!(any_model_ids(provider.get_all_models()), ["static", "a"]);

    refresh_provider(
        &provider,
        &store,
        RefreshOverrides {
            force: Some(true),
            ..default_allow_network()
        },
    )
    .await
    .expect("second refresh");
    let stored = read_store(&store).await.expect("stored");
    assert_eq!(stored.last_modified, Some(expected));
    assert_eq!(any_model_ids(provider.get_all_models()), ["static", "b"]);
}

/// 回归：无时区的 `Last-Modified` 按本地时间解释（V8 语义），解析结果必须参与新旧判断。
#[tokio::test]
async fn applies_catalog_with_zoneless_last_modified_as_local_time() {
    use chrono::TimeZone;

    let expected = chrono::Local
        .with_ymd_and_hms(2015, 10, 21, 7, 28, 0)
        .single()
        .expect("unambiguous local time")
        .timestamp_millis();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"zoneless": model_value("zoneless")}))
                .insert_header("last-modified", "Wed, 21 Oct 2015 07:28:00"),
        )
        .mount(&server)
        .await;

    let provider = test_provider(&server, Some(expected - 1));
    let store = Arc::new(InMemoryModelsStore::new());
    refresh_provider(&provider, &store, default_allow_network())
        .await
        .expect("refresh");
    let stored = read_store(&store).await.expect("stored");
    assert_eq!(stored.last_modified, Some(expected));
    assert_eq!(
        any_model_ids(provider.get_all_models()),
        ["static", "zoneless"]
    );
}

/// 回归：`10/21/2015` 这种 legacy 美式日期也是 V8 `Date.parse` 接受的形式（本地零点）。
#[tokio::test]
async fn applies_catalog_with_us_slash_date_last_modified() {
    use chrono::TimeZone;

    let expected = chrono::Local
        .with_ymd_and_hms(2015, 10, 21, 0, 0, 0)
        .single()
        .expect("unambiguous local time")
        .timestamp_millis();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"slash": model_value("slash")}))
                .insert_header("last-modified", "10/21/2015"),
        )
        .mount(&server)
        .await;

    let provider = test_provider(&server, Some(expected - 1));
    let store = Arc::new(InMemoryModelsStore::new());
    refresh_provider(&provider, &store, default_allow_network())
        .await
        .expect("refresh");
    let stored = read_store(&store).await.expect("stored");
    assert_eq!(stored.last_modified, Some(expected));
    assert_eq!(
        any_model_ids(provider.get_all_models()),
        ["static", "slash"]
    );
}

/// `new URL(path, catalogBaseUrl)` 失败在 TS 里是 `TypeError("Invalid URL")`（逐字）。
#[tokio::test]
async fn rejects_unparsable_catalog_base_url_with_invalid_url() {
    let signal = AbortScope::new();
    let error = request_catalog(PROVIDER_ID, "not a url", &signal, None)
        .await
        .expect_err("invalid base URL");
    assert_eq!(error.message, "Invalid URL");
    assert_eq!(error.name, "TypeError");
    assert_eq!(error.to_string(), "Invalid URL");
}
