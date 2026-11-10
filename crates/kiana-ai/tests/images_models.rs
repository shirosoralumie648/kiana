//! `packages/ai/test/images-models.test.ts` 的 Rust 移植。
//!
//! `models.generateImages` → [`kiana_ai::models::Models::generate_images`]；
//! `ModelsImagesOptions` 的头变换在 Rust 中是 [`kiana_ai::models::ModelsRequestTransforms`]。
//! TS 的 `getCompatModels("openrouter")` 对应 [`kiana_ai::compat::get_models`]（compat 单元已合入）。

mod models_support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::auth::resolve::AuthResolutionOverrides;
use kiana_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, AuthContext, AuthError, AuthResult, ModelAuth, ProviderAuth,
};
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::models::{
    CreateModelsOptions, CreateProviderOptions, GetAuthTarget, ImagesOptions, ModelsImagesOptions,
    ModelsRefreshOptions, ModelsRequestTransforms, ModelsStore, Provider, ProviderApiImpl,
    ProviderImages, ProviderStreams, TransformHeadersFn, create_models, create_provider, has_api,
    is_model_type,
};
use kiana_ai::providers::all::{
    builtin_models, get_all_builtin_models, get_builtin_classifier_models, get_builtin_image_model,
    get_builtin_image_models, get_builtin_models,
};
use kiana_ai::types::{
    AnyModel, AssistantImages, Context, ImageContent, ImagesContext, ImagesStopReason, Model,
    ModelCost, ModelInputModality, ModelType, SimpleStreamOptions, StreamOptions, TextContent,
    TranscriptContext, UserContentBlock,
};
use models_support::ambient_auth;

fn image_model(provider: &str, id: &str) -> Model {
    Model {
        model_type: Some(ModelType::Image),
        id: id.into(),
        name: id.into(),
        api: "test-images".into(),
        provider: provider.into(),
        base_url: "https://example.test/v1".into(),
        input: vec![ModelInputModality::Text],
        output: Some(vec!["image".to_string()]),
        cost: ModelCost::default(),
        ..Model::default()
    }
}

fn chat_model(provider: &str, id: &str) -> Model {
    Model {
        id: id.into(),
        name: id.into(),
        api: "test-chat".into(),
        provider: provider.into(),
        base_url: "https://example.test/v1".into(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(1000),
        max_tokens: Some(100),
        ..Model::default()
    }
}

fn images_context() -> ImagesContext {
    ImagesContext {
        input: vec![UserContentBlock::Text(TextContent {
            kind_text: Default::default(),
            text: "a red circle".into(),
            text_signature: None,
        })],
    }
}

fn ok_result(model: &Model) -> AssistantImages {
    AssistantImages {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        output: vec![UserContentBlock::Image(ImageContent {
            kind_image: Default::default(),
            data: "aGk=".into(),
            mime_type: "image/png".into(),
        })],
        response_id: None,
        usage: None,
        stop_reason: ImagesStopReason::Stop,
        error_message: None,
        timestamp: models_support::now_ms(),
    }
}

#[derive(Clone, Default)]
struct GenerateCall {
    api_key: Option<String>,
    env: Option<IndexMap<String, String>>,
    headers: Option<IndexMap<String, Option<String>>>,
}

type Calls = Arc<Mutex<Vec<GenerateCall>>>;

struct TestImages {
    calls: Calls,
}

#[async_trait]
impl ProviderImages for TestImages {
    async fn generate_images(
        &self,
        model: &Model,
        _context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> AssistantImages {
        self.calls.lock().unwrap().push(GenerateCall {
            api_key: options.and_then(|options| options.api_key.clone()),
            env: options.and_then(|options| options.env.clone()),
            headers: options.and_then(|options| options.headers.clone()),
        });
        ok_result(model)
    }
}

struct TestApiKeyAuth {
    env_var: Option<String>,
}

#[async_trait]
impl ApiKeyAuth for TestApiKeyAuth {
    fn name(&self) -> &str {
        "Test key"
    }

    async fn resolve(&self, input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        let Some(env_var) = &self.env_var else {
            return Ok(Some(AuthResult::new(ModelAuth::default())));
        };
        let key = match input
            .credential
            .and_then(|credential| credential.key.clone())
        {
            Some(key) => key,
            None => match input.ctx.env(env_var).await {
                Some(key) => key,
                None => return Ok(None),
            },
        };
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some(key),
                ..Default::default()
            },
            env: None,
            source: Some(if input.credential.is_some() {
                "stored".into()
            } else {
                env_var.clone()
            }),
        }))
    }
}

struct NoopStreams;

#[async_trait]
impl ProviderStreams for NoopStreams {
    fn stream(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: StreamOptions,
    ) -> AssistantMessageEventStream {
        AssistantMessageEventStream::new()
    }
    fn stream_simple(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        AssistantMessageEventStream::new()
    }
}

fn test_provider(
    id: &str,
    models: Option<Vec<AnyModel>>,
    env_var: Option<&str>,
    calls: Option<Calls>,
    images: Option<&[&str]>,
) -> Arc<dyn Provider> {
    let calls = calls.unwrap_or_default();
    let image_apis: Vec<String> = images
        .map(|apis| apis.iter().map(|api| (*api).to_string()).collect())
        .unwrap_or_else(|| vec!["test-images".to_string()]);
    let images = image_apis
        .into_iter()
        .map(|api| {
            (
                api,
                Arc::new(TestImages {
                    calls: calls.clone(),
                }) as Arc<dyn ProviderImages>,
            )
        })
        .collect::<IndexMap<_, _>>();
    create_provider(CreateProviderOptions {
        id: id.into(),
        auth: ProviderAuth {
            api_key: Some(Arc::new(TestApiKeyAuth {
                env_var: env_var.map(str::to_string),
            })),
            oauth: None,
        },
        models: models.unwrap_or_else(|| vec![AnyModel::Image(image_model(id, "model-a"))]),
        api: Some(ProviderApiImpl::ByApi(IndexMap::from([(
            "test-chat".to_string(),
            Arc::new(NoopStreams) as Arc<dyn ProviderStreams>,
        )]))),
        images: Some(images),
        ..Default::default()
    })
    .expect("provider")
}

struct FakeAuthContext {
    env: IndexMap<String, String>,
}

impl FakeAuthContext {
    fn new(entries: &[(&str, &str)]) -> Arc<Self> {
        Arc::new(Self {
            env: entries
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
        })
    }
}

#[async_trait]
impl AuthContext for FakeAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        self.env.get(name).cloned()
    }
    async fn file_exists(&self, _path: &str) -> bool {
        false
    }
}

fn model_ids(models: Vec<Model>) -> Vec<String> {
    models.into_iter().map(|model| model.id).collect()
}

fn all_model_ids(models: Vec<AnyModel>) -> Vec<String> {
    models
        .into_iter()
        .map(|model| model.model().id.clone())
        .collect()
}

fn as_chat(model: &Model) -> AnyModel {
    AnyModel::Chat(model.clone())
}

#[test]
fn treats_models_without_a_type_as_chat_models() {
    let chat = chat_model("p", "c");
    let image = image_model("p", "i");

    assert_eq!(chat.model_type, None);
    assert_eq!(
        kiana_ai::models::get_model_type(&as_chat(&chat)),
        ModelType::Chat
    );
    assert_eq!(
        kiana_ai::models::get_model_type(&AnyModel::Chat(Model {
            model_type: Some(ModelType::Chat),
            ..chat.clone()
        })),
        ModelType::Chat
    );
    assert_eq!(
        kiana_ai::models::get_model_type(&AnyModel::Image(image.clone())),
        ModelType::Image
    );
    assert!(is_model_type(&as_chat(&chat), ModelType::Chat));
    assert!(!is_model_type(&as_chat(&chat), ModelType::Image));
    assert!(is_model_type(
        &AnyModel::Image(image.clone()),
        ModelType::Image
    ));

    // hasApi never matches an image model, even on an equal api string
    assert!(!has_api(
        &AnyModel::Image(Model {
            api: "test-chat".into(),
            ..image
        }),
        "test-chat"
    ));
    assert!(has_api(&as_chat(&chat), "test-chat"));
}

#[tokio::test]
async fn lists_models_without_a_type_as_chat_models_at_create_provider_boundaries() {
    let provider = create_provider(CreateProviderOptions {
        id: "legacy".into(),
        auth: ambient_auth(),
        models: vec![
            AnyModel::Chat(chat_model("legacy", "static")),
            AnyModel::Image(image_model("legacy", "static")),
        ],
        fetch_models: Some(Arc::new(|_context| {
            Box::pin(async { Ok(vec![AnyModel::Chat(chat_model("legacy", "dynamic"))]) })
        })),
        api: Some(ProviderApiImpl::Single(Arc::new(NoopStreams))),
        ..Default::default()
    })
    .expect("provider");
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(provider.clone());

    assert_eq!(model_ids(provider.get_models()), vec!["static"]);
    models
        .refresh(ModelsRefreshOptions {
            providers: Some(vec![provider.id().to_string()]),
            ..Default::default()
        })
        .await
        .expect("refresh");
    assert_eq!(model_ids(provider.get_models()), vec!["static", "dynamic"]);
    assert_eq!(
        provider
            .get_models()
            .into_iter()
            .map(|model| model.model_type)
            .collect::<Vec<_>>(),
        vec![None, None]
    );
    assert_eq!(
        model_ids(models.get_models_of_type(ModelType::Image, Some("legacy"))),
        vec!["static"]
    );
}

#[test]
fn lists_chat_image_and_all_models_through_typed_accessors() {
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(test_provider(
        "p1",
        Some(vec![
            AnyModel::Chat(chat_model("p1", "c1")),
            AnyModel::Image(image_model("p1", "i1")),
            AnyModel::Image(image_model("p1", "i2")),
        ]),
        None,
        None,
        None,
    ));
    models.set_provider(test_provider(
        "p2",
        Some(vec![AnyModel::Image(image_model("p2", "i3"))]),
        None,
        None,
        None,
    ));

    assert_eq!(model_ids(models.get_models(None)), vec!["c1"]);
    assert_eq!(
        model_ids(models.get_models_of_type(ModelType::Chat, None)),
        vec!["c1"]
    );
    assert_eq!(
        model_ids(models.get_models_of_type(ModelType::Image, None)),
        vec!["i1", "i2", "i3"]
    );
    assert_eq!(
        model_ids(models.get_models_of_type(ModelType::Image, Some("p1"))),
        vec!["i1", "i2"]
    );
    assert!(
        models
            .get_models_of_type(ModelType::Classifier, None)
            .is_empty()
    );
    assert_eq!(
        all_model_ids(models.get_all_models(None)),
        vec!["c1", "i1", "i2", "i3"]
    );

    assert_eq!(models.get_model("p1", "c1").expect("c1").id, "c1");
    assert!(models.get_model("p1", "i1").is_none());
    assert_eq!(
        models
            .get_model_of_type(ModelType::Chat, "p1", "c1")
            .expect("c1")
            .id,
        "c1"
    );
    assert_eq!(
        models
            .get_model_of_type(ModelType::Image, "p1", "i1")
            .expect("i1")
            .id,
        "i1"
    );
    assert!(
        models
            .get_model_of_type(ModelType::Image, "p1", "c1")
            .is_none()
    );
}

#[tokio::test]
async fn splits_available_models_by_type() {
    let models = create_models(CreateModelsOptions {
        auth_context: Some(FakeAuthContext::new(&[("KEY", "k")])),
        ..Default::default()
    });
    models.set_provider(test_provider(
        "p1",
        Some(vec![
            AnyModel::Chat(chat_model("p1", "c1")),
            AnyModel::Image(image_model("p1", "i1")),
        ]),
        Some("KEY"),
        None,
        None,
    ));
    models.set_provider(test_provider(
        "p2",
        Some(vec![AnyModel::Image(image_model("p2", "i2"))]),
        Some("MISSING"),
        None,
        None,
    ));

    assert_eq!(
        model_ids(
            models
                .get_available(None, Default::default())
                .await
                .expect("available")
        ),
        vec!["c1"]
    );
    assert_eq!(
        model_ids(
            models
                .get_available_of_type(ModelType::Chat, None, Default::default())
                .await
                .expect("chat")
        ),
        vec!["c1"]
    );
    assert_eq!(
        model_ids(
            models
                .get_available_of_type(ModelType::Image, None, Default::default())
                .await
                .expect("image")
        ),
        vec!["i1"]
    );
    assert_eq!(
        all_model_ids(
            models
                .get_all_available(None, Default::default())
                .await
                .expect("all")
        ),
        vec!["c1", "i1"]
    );
}

#[tokio::test]
async fn resolves_auth_through_the_provider_and_merges_it_into_image_requests() {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let models = create_models(CreateModelsOptions {
        auth_context: Some(FakeAuthContext::new(&[("TEST_KEY", "env-key")])),
        ..Default::default()
    });
    models.set_provider(test_provider(
        "p1",
        None,
        Some("TEST_KEY"),
        Some(calls.clone()),
        None,
    ));
    let model = models
        .get_model_of_type(ModelType::Image, "p1", "model-a")
        .expect("model");

    assert_eq!(
        models
            .get_auth(GetAuthTarget::Model(&model), Default::default())
            .await
            .expect("auth")
            .expect("configured")
            .auth
            .api_key,
        Some("env-key".into())
    );
    assert_eq!(
        models
            .get_auth(
                GetAuthTarget::Provider(model.provider.as_str()),
                Default::default()
            )
            .await
            .expect("auth")
            .expect("configured")
            .auth
            .api_key,
        Some("env-key".into())
    );
    assert_eq!(
        models
            .get_auth(
                GetAuthTarget::Model(&model),
                AuthResolutionOverrides {
                    api_key: Some("explicit-key".into()),
                    ..Default::default()
                }
            )
            .await
            .expect("auth")
            .expect("configured")
            .auth
            .api_key,
        Some("explicit-key".into())
    );

    let result = models
        .generate_images(&model, &images_context(), ModelsImagesOptions::default())
        .await;
    assert_eq!(result.stop_reason, ImagesStopReason::Stop);
    assert_eq!(calls.lock().unwrap()[0].api_key.as_deref(), Some("env-key"));

    models
        .generate_images(
            &model,
            &images_context(),
            ModelsImagesOptions {
                base: StreamOptions {
                    api_key: Some("explicit".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        calls.lock().unwrap()[1].api_key.as_deref(),
        Some("explicit")
    );
}

#[tokio::test]
async fn merges_provider_resolved_env_and_applies_header_transforms() {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let models = create_models(CreateModelsOptions::default());
    let provider_calls = calls.clone();
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "p1".into(),
            auth: ProviderAuth {
                api_key: Some(Arc::new(ProviderEnvAuth)),
                oauth: None,
            },
            models: vec![AnyModel::Image(image_model("p1", "model-a"))],
            images: Some(IndexMap::from([(
                "test-images".to_string(),
                Arc::new(TestImages {
                    calls: provider_calls,
                }) as Arc<dyn ProviderImages>,
            )])),
            ..Default::default()
        })
        .expect("provider"),
    );
    let model = models
        .get_model_of_type(ModelType::Image, "p1", "model-a")
        .expect("model");

    let transform: TransformHeadersFn = Arc::new(|mut headers| {
        headers.insert("x-extra".into(), Some("2".into()));
        Box::pin(async move { headers })
    });
    models
        .generate_images(
            &model,
            &images_context(),
            ModelsImagesOptions {
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
                transforms: ModelsRequestTransforms {
                    transform_headers: Some(transform),
                },
                ..Default::default()
            },
        )
        .await;

    let calls = calls.lock().unwrap();
    assert_eq!(calls[0].api_key.as_deref(), Some("request-key"));
    assert_eq!(
        calls[0].env,
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
    assert_eq!(
        calls[0].headers,
        Some(
            [
                ("x-base".to_string(), Some("1".to_string())),
                ("x-extra".to_string(), Some("2".to_string())),
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
        "Test key"
    }
    async fn resolve(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some("provider-key".into()),
                headers: Some(
                    [("x-base".to_string(), Some("1".to_string()))]
                        .into_iter()
                        .collect(),
                ),
                base_url: None,
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

#[tokio::test]
async fn returns_error_results_instead_of_rejecting() {
    let models = create_models(CreateModelsOptions {
        auth_context: Some(FakeAuthContext::new(&[])),
        ..Default::default()
    });

    let ghost = models
        .generate_images(
            &image_model("ghost", "m"),
            &images_context(),
            Default::default(),
        )
        .await;
    assert_eq!(ghost.stop_reason, ImagesStopReason::Error);
    assert!(
        ghost
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("Unknown provider: ghost")
    );

    // Unconfigured auth is an error, matching stream().
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    models.set_provider(test_provider(
        "p1",
        None,
        Some("MISSING"),
        Some(calls.clone()),
        None,
    ));
    let model = models
        .get_model_of_type(ModelType::Image, "p1", "model-a")
        .expect("model");
    assert!(
        models
            .get_auth(GetAuthTarget::Model(&model), Default::default())
            .await
            .expect("auth")
            .is_none()
    );
    let unconfigured = models
        .generate_images(&model, &images_context(), Default::default())
        .await;
    assert_eq!(unconfigured.stop_reason, ImagesStopReason::Error);
    assert!(
        unconfigured
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("not configured")
    );
    assert!(calls.lock().unwrap().is_empty());

    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let cancelled = models
        .generate_images(
            &model,
            &images_context(),
            ModelsImagesOptions {
                base: StreamOptions {
                    signal: Some(token),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;
    assert_eq!(cancelled.stop_reason, ImagesStopReason::Aborted);
    assert!(calls.lock().unwrap().is_empty());

    // A provider without any images implementation rejects image models it lists.
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "chat-only".into(),
            auth: ambient_auth(),
            models: vec![AnyModel::Image(image_model("chat-only", "i"))],
            api: Some(ProviderApiImpl::Single(Arc::new(NoopStreams))),
            ..Default::default()
        })
        .expect("provider"),
    );
    let unsupported = models
        .generate_images(
            &models
                .get_model_of_type(ModelType::Image, "chat-only", "i")
                .expect("image"),
            &images_context(),
            Default::default(),
        )
        .await;
    assert_eq!(unsupported.stop_reason, ImagesStopReason::Error);
    assert!(
        unsupported
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("does not support image generation")
    );

    // An images map without the model's api yields a provider error result.
    models.set_provider(test_provider(
        "wrong-api",
        Some(vec![AnyModel::Image(image_model("wrong-api", "i"))]),
        None,
        None,
        Some(&["other-images"]),
    ));
    let missing_api = models
        .generate_images(
            &models
                .get_model_of_type(ModelType::Image, "wrong-api", "i")
                .expect("image"),
            &images_context(),
            Default::default(),
        )
        .await;
    assert_eq!(missing_api.stop_reason, ImagesStopReason::Error);
    assert!(
        missing_api
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("no image generation implementation for \"test-images\"")
    );
}

#[tokio::test]
async fn rejects_chat_models_at_the_image_entry_point_at_runtime() {
    let models = create_models(CreateModelsOptions::default());
    let chat = chat_model("p1", "chat");
    models.set_provider(test_provider(
        "p1",
        Some(vec![AnyModel::Chat(chat.clone())]),
        None,
        None,
        Some(&["test-chat"]),
    ));

    let result = models
        .generate_images(&chat, &images_context(), Default::default())
        .await;
    assert_eq!(result.stop_reason, ImagesStopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("is not an image model")
    );
}

#[tokio::test]
async fn rejects_image_models_at_the_stream_entry_points_at_runtime() {
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(test_provider("p1", None, None, None, None));
    let image = models
        .get_model_of_type(ModelType::Image, "p1", "model-a")
        .expect("image");

    let result = models
        .stream_simple(&image, &Context::default(), Default::default())
        .result()
        .await
        .expect("final message");
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("is not a chat model")
    );
}

#[test]
#[allow(clippy::result_large_err)]
fn requires_at_least_one_concrete_operation_implementation() {
    let create_empty_provider = |api: Option<ProviderApiImpl>,
                                 images: Option<IndexMap<String, Arc<dyn ProviderImages>>>,
                                 classifiers: Option<
        IndexMap<String, Arc<dyn kiana_ai::models::ProviderClassifier>>,
    >| {
        create_provider(CreateProviderOptions {
            id: "empty".into(),
            auth: ambient_auth(),
            models: Vec::new(),
            api,
            images,
            classifiers,
            ..Default::default()
        })
    };

    let message = "at least one of \"api\", \"images\", or \"classifiers\"";
    for error in [
        create_empty_provider(None, None, None).err().expect("none"),
        create_empty_provider(Some(ProviderApiImpl::ByApi(IndexMap::new())), None, None)
            .err()
            .expect("empty api"),
        create_empty_provider(None, Some(IndexMap::new()), None)
            .err()
            .expect("empty images"),
        create_empty_provider(None, None, Some(IndexMap::new()))
            .err()
            .expect("empty classifiers"),
    ] {
        assert!(format!("{error}").contains(message), "{error}");
    }
}

#[tokio::test]
async fn supports_dynamic_providers_listing_image_models_via_refresh() {
    let fetches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fetches_for_fetch = fetches.clone();
    let models_store = Arc::new(kiana_ai::models::InMemoryModelsStore::new());
    let models = create_models(CreateModelsOptions {
        models_store: Some(models_store.clone()),
        ..Default::default()
    });
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "dyn".into(),
            auth: ambient_auth(),
            models: Vec::new(),
            fetch_models: Some(Arc::new(move |_context| {
                let fetches = fetches_for_fetch.clone();
                Box::pin(async move {
                    fetches.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(vec![
                        AnyModel::Image(image_model("dyn", "listed")),
                        AnyModel::Chat(chat_model("dyn", "chat")),
                    ])
                })
            })),
            images: Some(IndexMap::from([(
                "test-images".to_string(),
                Arc::new(TestImages {
                    calls: Calls::default(),
                }) as Arc<dyn ProviderImages>,
            )])),
            ..Default::default()
        })
        .expect("provider"),
    );

    assert!(models.get_all_models(Some("dyn")).is_empty());
    let result = models
        .refresh(ModelsRefreshOptions {
            providers: Some(vec!["dyn".into()]),
            ..Default::default()
        })
        .await
        .expect("refresh");
    assert!(result.errors.is_empty());
    assert_eq!(fetches.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(
        models
            .get_model_of_type(ModelType::Image, "dyn", "listed")
            .is_some()
    );
    assert!(models.get_model("dyn", "chat").is_some());
    let stored = models_store
        .read("dyn", Default::default())
        .await
        .expect("read")
        .expect("stored catalog");
    assert_eq!(all_model_ids(stored.models), vec!["listed", "chat"]);
}

#[test]
fn keeps_existing_builtin_model_reads_chat_only() {
    let chat = get_builtin_models("openrouter");
    let images = get_builtin_image_models("openrouter");
    let all = get_all_builtin_models("openrouter");
    let compat = kiana_ai::compat::get_models("openrouter");

    assert!(
        chat.iter()
            .all(|model| is_model_type(&as_chat(model), ModelType::Chat))
    );
    assert!(
        images
            .iter()
            .all(|model| is_model_type(&AnyModel::Image(model.clone()), ModelType::Image))
    );
    assert!(
        all.iter()
            .any(|model| is_model_type(model, ModelType::Image))
    );
    assert_eq!(compat, chat);
    assert!(
        chat.iter()
            .all(|model| model.context_window.is_some_and(|window| window > 0))
    );
    assert_eq!(
        chat.len() + images.len() + get_builtin_classifier_models("openrouter").len(),
        all.len()
    );
    assert_eq!(
        get_builtin_image_model("openrouter", "black-forest-labs/flux.2-pro")
            .expect("flux.2-pro")
            .model_type,
        Some(ModelType::Image)
    );
}

#[tokio::test]
async fn builtin_models_exposes_openrouter_image_models_under_the_openrouter_provider() {
    let models = builtin_models(Some(CreateModelsOptions {
        auth_context: Some(FakeAuthContext::new(&[("OPENROUTER_API_KEY", "or-key")])),
        ..Default::default()
    }));
    let provider = models
        .get_provider("openrouter")
        .expect("openrouter provider");
    let images = models.get_models_of_type(ModelType::Image, Some("openrouter"));
    assert!(!images.is_empty());
    assert!(
        provider
            .get_models()
            .iter()
            .all(|model| is_model_type(&as_chat(model), ModelType::Chat))
    );
    assert!(
        provider
            .get_all_models()
            .iter()
            .any(|model| is_model_type(model, ModelType::Image))
    );
    assert!(images.iter().all(
        |model| model.model_type == Some(ModelType::Image) && model.api == "openrouter-images"
    ));
    assert!(
        models
            .get_models_of_type(ModelType::Image, None)
            .iter()
            .all(|model| model.provider == "openrouter")
    );

    // One upstream id can expose separate chat and image operations.
    let chat = models
        .get_model("openrouter", "google/gemini-3-pro-image")
        .expect("chat model");
    let image = models
        .get_model_of_type(ModelType::Image, "openrouter", "google/gemini-3-pro-image")
        .expect("image model");
    assert_eq!(chat.api, "openai-completions");
    assert_eq!(image.api, "openrouter-images");

    // One credential covers both.
    assert_eq!(
        models
            .get_auth(GetAuthTarget::Model(&images[0]), Default::default())
            .await
            .expect("auth")
            .expect("configured")
            .auth
            .api_key,
        Some("or-key".into())
    );
    assert_eq!(
        models
            .get_auth(GetAuthTarget::Model(&chat), Default::default())
            .await
            .expect("auth")
            .expect("configured")
            .auth
            .api_key,
        Some("or-key".into())
    );
    assert!(provider.has_generate_images());
}
