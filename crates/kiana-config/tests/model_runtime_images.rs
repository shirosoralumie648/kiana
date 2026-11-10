//! `model-runtime-images.test.ts` 的移植（7 个用例）。

#[path = "fixtures/model_runtime/mod.rs"]
mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::auth::types::{AuthResult, ModelAuth, ProviderAuth};
use kiana_ai::models::{
    ClassifierOptions, CreateProviderOptions, ImagesOptions, ProviderClassifier, ProviderImages,
    ProviderStreams,
};
use kiana_ai::types::{
    AnyModel, AssistantImages, ClassifierAnswer, ClassifierContext, ClassifierResult,
    DeferredHandle, ImagesContext, ImagesStopReason, Model, ModelType, StopReason, StreamOptions,
    TranscriptContext,
};
use kiana_config::models::composer::{ProviderConfigInput, ProviderModelConfig};
use serde_json::json;
use support::*;

/// `(apiKey, headers)` 记录。
type RecordedCalls = Arc<Mutex<Vec<(Option<String>, Option<kiana_ai::types::ProviderHeaders>)>>>;

/// 记录 `generateImages` 收到的请求。
#[derive(Default)]
struct RecordingImages {
    calls: RecordedCalls,
}

#[async_trait]
impl ProviderImages for RecordingImages {
    async fn generate_images(
        &self,
        model: &Model,
        _context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> AssistantImages {
        self.calls.lock().unwrap().push((
            options.and_then(|options| options.api_key.clone()),
            options.and_then(|options| options.headers.clone()),
        ));
        AssistantImages {
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            output: Vec::new(),
            stop_reason: ImagesStopReason::Stop,
            ..Default::default()
        }
    }
}

/// 记录 `classify` 收到的请求。
#[derive(Default)]
struct RecordingClassifier {
    calls: RecordedCalls,
}

#[async_trait]
impl ProviderClassifier for RecordingClassifier {
    async fn classify(
        &self,
        model: &Model,
        _context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> ClassifierResult {
        self.calls.lock().unwrap().push((
            options.and_then(|options| options.api_key.clone()),
            options.and_then(|options| options.headers.clone()),
        ));
        let mut answers = IndexMap::new();
        answers.insert(
            "approved".to_string(),
            ClassifierAnswer::Bool { probability: 0.9 },
        );
        ClassifierResult {
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            answers,
            stop_reason: kiana_ai::types::ClassifierStopReason::Stop,
            ..Default::default()
        }
    }
}

/// 只用来计数分发次数的 chat 流实现。
struct CountingStreams {
    dispatches: Arc<AtomicUsize>,
}

impl ProviderStreams for CountingStreams {
    fn stream(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: StreamOptions,
    ) -> kiana_ai::event_stream::AssistantMessageEventStream {
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        kiana_ai::event_stream::AssistantMessageEventStream::new()
    }

    fn stream_simple(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: kiana_ai::types::SimpleStreamOptions,
    ) -> kiana_ai::event_stream::AssistantMessageEventStream {
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        kiana_ai::event_stream::AssistantMessageEventStream::new()
    }

    fn has_fetch_deferred(&self) -> bool {
        true
    }

    fn fetch_deferred(
        &self,
        _model: &Model,
        _handle: &DeferredHandle,
        _options: kiana_ai::types::DeferredFetchOptions,
    ) -> Option<kiana_ai::event_stream::AssistantMessageEventStream> {
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        Some(kiana_ai::event_stream::AssistantMessageEventStream::new())
    }
}

fn resolved_into_auth(auth: ModelAuth) -> AuthResult {
    AuthResult::new(auth)
}

fn api_key_auth(name: &str) -> ProviderAuth {
    let auth = TestApiKeyAuth::new(name).with_resolve(Arc::new(|_credential| {
        Box::pin(async { Ok(Some(resolved_into_auth(ModelAuth::default()))) })
    }));
    Arc::new(auth).provider_auth()
}

fn stored_key_auth(name: &str) -> ProviderAuth {
    let auth = TestApiKeyAuth::new(name).with_resolve(Arc::new(|credential| {
        Box::pin(async move {
            Ok(credential.and_then(|credential| {
                credential.key.map(|key| {
                    AuthResult::new(ModelAuth {
                        api_key: Some(key),
                        ..Default::default()
                    })
                })
            }))
        })
    }));
    Arc::new(auth).provider_auth()
}

#[tokio::test]
async fn lists_builtin_open_router_image_models_separately_from_chat_models() {
    let harness = Harness::new();
    let runtime = harness.create().await;

    let images = runtime.get_models_of_type(ModelType::Image, Some("openrouter"));
    assert!(!images.is_empty());
    assert!(
        runtime
            .get_models(Some("openrouter"))
            .iter()
            .all(|model| model.model_type.unwrap_or(ModelType::Chat) == ModelType::Chat)
    );
    assert_eq!(
        runtime.get_model_of_type(ModelType::Image, "openrouter", &images[0].id),
        Some(images[0].clone())
    );
    assert_eq!(
        runtime
            .get_model("openrouter", "google/gemini-3-pro-image")
            .expect("chat model")
            .api,
        "openai-completions"
    );
    assert_eq!(
        runtime
            .get_model_of_type(ModelType::Image, "openrouter", "google/gemini-3-pro-image")
            .expect("image model")
            .model_type,
        Some(ModelType::Image)
    );
    let classifiers = runtime.get_models_of_type(ModelType::Classifier, Some("openrouter"));
    assert_eq!(
        runtime.get_all_models(Some("openrouter")).len(),
        runtime.get_models(Some("openrouter")).len() + images.len() + classifiers.len()
    );
}

#[tokio::test]
async fn extension_model_lists_replace_undeclared_models_of_every_operation() {
    let harness = Harness::new();
    let runtime = harness.create().await;

    let chat = Model {
        id: "built-in-chat".to_string(),
        name: "Built-in chat".to_string(),
        api: "test-chat".to_string(),
        provider: "mixed".to_string(),
        base_url: "https://built-in.test/v1".to_string(),
        reasoning: Some(false),
        input: vec![kiana_ai::types::ModelInputModality::Text],
        context_window: Some(1000),
        max_tokens: Some(100),
        ..chat_model("mixed", "built-in-chat")
    };
    let built_in = kiana_ai::models::create_provider(CreateProviderOptions {
        id: "mixed".to_string(),
        auth: api_key_auth("Mixed key"),
        models: vec![
            AnyModel::Chat(chat),
            AnyModel::Image(image_model("mixed", "built-in-image")),
            AnyModel::Classifier(classifier_model("mixed", "built-in-classifier")),
        ],
        images: Some(IndexMap::from([(
            "test-images".to_string(),
            Arc::new(RecordingImages::default()) as Arc<dyn ProviderImages>,
        )])),
        ..Default::default()
    })
    .expect("mixed provider");
    runtime
        .register_native_provider(built_in)
        .expect("register native");

    runtime
        .register_provider(
            "mixed",
            ProviderConfigInput {
                api_key: Some("extension-secret".to_string()),
                models: Some(vec![ProviderModelConfig::Chat(
                    kiana_config::models::composer::ProviderChatModelConfig {
                        base: kiana_config::models::composer::ProviderModelConfigBase {
                            id: "extension-chat".to_string(),
                            name: "Extension chat".to_string(),
                            api: Some("test-chat".to_string()),
                            base_url: Some("https://chat-proxy.test/v1".to_string()),
                            input: vec![kiana_ai::types::ModelInputModality::Text],
                            input_limits: None,
                            cost: kiana_ai::types::ModelCost::default(),
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
                    },
                )]),
                ..Default::default()
            },
        )
        .expect("register extension");

    let all: Vec<(String, String)> = runtime
        .get_all_models(Some("mixed"))
        .iter()
        .map(|model| {
            (
                match model.model_type() {
                    ModelType::Chat => "chat".to_string(),
                    ModelType::Image => "image".to_string(),
                    ModelType::Classifier => "classifier".to_string(),
                },
                model.model().id.clone(),
            )
        })
        .collect();
    assert_eq!(
        all,
        vec![("chat".to_string(), "extension-chat".to_string())]
    );
    assert_eq!(
        runtime
            .get_model("mixed", "extension-chat")
            .expect("extension chat")
            .base_url,
        "https://chat-proxy.test/v1"
    );
    assert!(
        runtime
            .get_models_of_type(ModelType::Image, Some("mixed"))
            .is_empty()
    );
    assert!(
        runtime
            .get_models_of_type(ModelType::Classifier, Some("mixed"))
            .is_empty()
    );
}

#[tokio::test]
async fn registers_extension_image_and_classifier_models_with_their_implementations() {
    let harness = Harness::new();
    let runtime = harness.create().await;
    let images = Arc::new(RecordingImages::default());
    let classifiers = Arc::new(RecordingClassifier::default());

    runtime
        .register_provider(
            "extension-operations",
            ProviderConfigInput {
                api_key: Some("extension-secret".to_string()),
                models: Some(vec![
                    ProviderModelConfig::Image(
                        kiana_config::models::composer::ProviderImageModelConfig {
                            base: kiana_config::models::composer::ProviderModelConfigBase {
                                id: "shared".to_string(),
                                name: "shared".to_string(),
                                api: Some("test-images".to_string()),
                                base_url: Some("https://images.test/v1".to_string()),
                                input: vec![kiana_ai::types::ModelInputModality::Text],
                                input_limits: None,
                                cost: kiana_ai::types::ModelCost::default(),
                                headers: Some(IndexMap::from([(
                                    "X-Operation".to_string(),
                                    "image".to_string(),
                                )])),
                                declared_type: Some(ModelType::Image),
                            },
                            output: vec![kiana_ai::types::ModelInputModality::Image],
                        },
                    ),
                    ProviderModelConfig::Classifier(
                        kiana_config::models::composer::ProviderClassifierModelConfig {
                            base: kiana_config::models::composer::ProviderModelConfigBase {
                                id: "shared".to_string(),
                                name: "shared".to_string(),
                                api: Some("test-classifier".to_string()),
                                base_url: Some("https://classifier.test/v1".to_string()),
                                input: vec![kiana_ai::types::ModelInputModality::Text],
                                input_limits: None,
                                cost: kiana_ai::types::ModelCost::default(),
                                headers: Some(IndexMap::from([(
                                    "X-Operation".to_string(),
                                    "classifier".to_string(),
                                )])),
                                declared_type: Some(ModelType::Classifier),
                            },
                            context_window: 1000.0,
                            compat: None,
                        },
                    ),
                ]),
                images: IndexMap::from([(
                    "test-images".to_string(),
                    Arc::clone(&images) as Arc<dyn ProviderImages>,
                )]),
                classifiers: IndexMap::from([(
                    "test-classifier".to_string(),
                    Arc::clone(&classifiers) as Arc<dyn ProviderClassifier>,
                )]),
                ..Default::default()
            },
        )
        .expect("register");

    let image = runtime
        .get_model_of_type(ModelType::Image, "extension-operations", "shared")
        .expect("image model");
    let classifier = runtime
        .get_model_of_type(ModelType::Classifier, "extension-operations", "shared")
        .expect("classifier model");

    let image_result = runtime
        .generate_images(&image, &ImagesContext::default(), Default::default())
        .await;
    assert_eq!(image_result.stop_reason, ImagesStopReason::Stop);
    let classifier_result = runtime
        .classify(
            &classifier,
            &ClassifierContext::default(),
            Default::default(),
        )
        .await;
    assert_eq!(
        classifier_result.stop_reason,
        kiana_ai::types::ClassifierStopReason::Stop
    );

    let observed: Vec<(Option<String>, Option<kiana_ai::types::ProviderHeaders>)> = images
        .calls
        .lock()
        .unwrap()
        .clone()
        .into_iter()
        .chain(classifiers.calls.lock().unwrap().clone())
        .collect();
    assert_eq!(observed.len(), 2);
    for (index, (api_key, headers)) in observed.iter().enumerate() {
        assert_eq!(api_key.as_deref(), Some("extension-secret"));
        let headers = headers.as_ref().expect("headers");
        assert_eq!(
            header_value(headers, "X-Operation"),
            Some(Some(if index == 0 { "image" } else { "classifier" }))
        );
    }
}

#[tokio::test]
async fn generates_images_through_a_native_provider_with_runtime_resolved_auth() {
    let harness = Harness::new();
    let runtime = harness.create().await;
    let images = Arc::new(RecordingImages::default());
    runtime
        .register_native_provider(
            kiana_ai::models::create_provider(CreateProviderOptions {
                id: "pixels".to_string(),
                auth: stored_key_auth("Pixels key"),
                models: vec![AnyModel::Image(image_model("pixels", "flux"))],
                images: Some(IndexMap::from([(
                    "test-images".to_string(),
                    Arc::clone(&images) as Arc<dyn ProviderImages>,
                )])),
                ..Default::default()
            })
            .expect("pixels provider"),
        )
        .expect("register");
    let model = runtime
        .get_model_of_type(ModelType::Image, "pixels", "flux")
        .expect("flux");

    let unconfigured = runtime
        .generate_images(&model, &ImagesContext::default(), Default::default())
        .await;
    assert_eq!(unconfigured.stop_reason, ImagesStopReason::Error);
    assert!(
        unconfigured
            .error_message
            .as_deref()
            .expect("error message")
            .contains("not configured")
    );
    assert!(images.calls.lock().unwrap().is_empty());

    let controller = kiana_ai::utils::abort::AbortScope::new();
    controller.abort();
    let cancelled = runtime
        .generate_images(
            &model,
            &ImagesContext::default(),
            kiana_ai::models::ModelsImagesOptions {
                base: StreamOptions {
                    signal: Some(controller.token()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;
    assert_eq!(cancelled.stop_reason, ImagesStopReason::Aborted);
    assert!(images.calls.lock().unwrap().is_empty());

    runtime
        .set_runtime_api_key("pixels", "sk-pixels", Default::default())
        .await
        .expect("runtime key");
    let available: Vec<String> = runtime
        .get_available_of_type(ModelType::Image, Some("pixels"), Default::default())
        .await
        .expect("available")
        .into_iter()
        .map(|model| model.id)
        .collect();
    assert_eq!(available, vec!["flux".to_string()]);

    let result = runtime
        .generate_images(&model, &ImagesContext::default(), Default::default())
        .await;
    assert_eq!(result.stop_reason, ImagesStopReason::Stop);
    let calls = images.calls.lock().unwrap();
    assert_eq!(calls[0].0.as_deref(), Some("sk-pixels"));
}

#[tokio::test]
async fn rejects_image_models_at_every_chat_entry_point_before_provider_dispatch() {
    let harness = Harness::new();
    let runtime = harness.create().await;
    let dispatches = Arc::new(AtomicUsize::new(0));
    runtime
        .register_native_provider(
            kiana_ai::models::create_provider(CreateProviderOptions {
                id: "hybrid".to_string(),
                auth: stored_key_auth("Hybrid key"),
                models: vec![AnyModel::Image(image_model("hybrid", "shared"))],
                api: Some(kiana_ai::models::ProviderApiImpl::Single(Arc::new(
                    CountingStreams {
                        dispatches: Arc::clone(&dispatches),
                    },
                ))),
                ..Default::default()
            })
            .expect("hybrid provider"),
        )
        .expect("register");
    runtime
        .set_runtime_api_key("hybrid", "sk-hybrid", Default::default())
        .await
        .expect("runtime key");
    let image = runtime
        .get_model_of_type(ModelType::Image, "hybrid", "shared")
        .expect("image model");
    let chat = image.clone();
    let chat_context = kiana_ai::types::Context::default();
    let handle = DeferredHandle {
        provider: "hybrid".to_string(),
        model_id: image.id.clone(),
        api: image.api.clone(),
        id: "response-1".to_string(),
        expires_at: None,
        poll_after_ms: None,
        data: None,
    };

    let mut results = Vec::new();
    results.push(
        runtime
            .stream(&chat, &chat_context, Default::default())
            .result()
            .await
            .expect("stream result"),
    );
    results.push(
        runtime
            .complete(&chat, &chat_context, Default::default())
            .await,
    );
    results.push(
        runtime
            .stream_simple(&chat, &chat_context, Default::default())
            .result()
            .await
            .expect("stream simple result"),
    );
    results.push(
        runtime
            .complete_simple(&chat, &chat_context, Default::default())
            .await,
    );
    results.push(
        runtime
            .stream_deferred(&chat, &handle, Default::default())
            .result()
            .await
            .expect("stream deferred result"),
    );
    results.push(
        runtime
            .fetch_deferred(&chat, &handle, Default::default())
            .await,
    );
    for result in results {
        assert_eq!(result.stop_reason, StopReason::Error);
        assert!(
            result
                .error_message
                .as_deref()
                .expect("error message")
                .contains("is not a chat model")
        );
    }
    let error = runtime
        .cancel_deferred(&chat, &handle, Default::default())
        .await
        .expect_err("cancel error");
    assert!(error.message.contains("is not a chat model"));
    assert_eq!(dispatches.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn keeps_image_generation_on_a_builtin_provider_composed_with_models_json_overrides() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "openrouter": {
            "headers": { "X-Title": "kiana" },
            "modelOverrides": {
                "openrouter/auto": { "name": "Auto (renamed)" },
                "google/gemini-3-pro-image": { "headers": { "X-Chat-Only": "yes" } }
            }
        }}
    }));
    let runtime = harness.create().await;

    let provider = runtime.get_provider("openrouter").expect("provider");
    assert!(provider.has_generate_images());
    assert_eq!(
        runtime
            .get_model("openrouter", "openrouter/auto")
            .expect("auto")
            .name,
        "Auto (renamed)"
    );
    assert!(
        !runtime
            .get_models_of_type(ModelType::Image, Some("openrouter"))
            .is_empty()
    );

    runtime
        .set_runtime_api_key("openrouter", "sk-or", Default::default())
        .await
        .expect("runtime key");
    let chat = runtime
        .get_model("openrouter", "google/gemini-3-pro-image")
        .expect("chat model");
    let image = runtime
        .get_model_of_type(ModelType::Image, "openrouter", "google/gemini-3-pro-image")
        .expect("image model");
    let chat_auth = runtime
        .get_auth_for_model(&chat, Default::default())
        .await
        .expect("chat auth")
        .expect("chat configured");
    let image_auth = runtime
        .get_auth_for_model(&image, Default::default())
        .await
        .expect("image auth")
        .expect("image configured");

    assert_eq!(image_auth.auth.api_key.as_deref(), Some("sk-or"));
    let chat_headers = chat_auth.auth.headers.expect("chat headers");
    assert_eq!(header_value(&chat_headers, "X-Title"), Some(Some("kiana")));
    assert_eq!(
        header_value(&chat_headers, "X-Chat-Only"),
        Some(Some("yes"))
    );
    let image_headers = image_auth.auth.headers.expect("image headers");
    assert_eq!(header_value(&image_headers, "X-Title"), Some(Some("kiana")));
    assert_eq!(
        image_headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case("X-Chat-Only"))
            .count(),
        0
    );
}

#[tokio::test]
async fn does_not_add_image_generation_or_classification_to_composed_chat_only_providers() {
    let harness = Harness::new();
    harness.write_raw(json!({
        "providers": { "anthropic": { "headers": { "X-Title": "kiana" } } }
    }));
    let runtime = harness.create().await;

    let anthropic = runtime.get_provider("anthropic").expect("anthropic");
    assert!(!anthropic.has_generate_images());
    assert!(!anthropic.has_classify());
    assert!(
        runtime
            .get_provider("openrouter")
            .expect("openrouter")
            .has_generate_images()
    );
    assert!(
        runtime
            .get_provider("typesafe")
            .expect("typesafe")
            .has_classify()
    );
}

/// TS 的 `registerProvider` 逐键合并只看 `value !== undefined`：显式 `images: {}` /
/// `classifiers: {}` 会**清空**旧实现（Rust 用 `images_defined`/`classifiers_defined`
/// 表达「显式给出空表」）。
#[tokio::test]
async fn re_registering_with_explicit_empty_operation_maps_clears_implementations() {
    let harness = Harness::new();
    let runtime = harness.create().await;
    let images = Arc::new(RecordingImages::default());
    let classifiers = Arc::new(RecordingClassifier::default());
    let base = |id: &str, api: &str, declared_type: ModelType| {
        kiana_config::models::composer::ProviderModelConfigBase {
            id: id.to_string(),
            name: id.to_string(),
            api: Some(api.to_string()),
            base_url: Some("https://operations.test/v1".to_string()),
            input: vec![kiana_ai::types::ModelInputModality::Text],
            input_limits: None,
            cost: kiana_ai::types::ModelCost::default(),
            headers: None,
            declared_type: Some(declared_type),
        }
    };

    runtime
        .register_provider(
            "clear-operations",
            ProviderConfigInput {
                api_key: Some("extension-secret".to_string()),
                models: Some(vec![
                    ProviderModelConfig::Image(
                        kiana_config::models::composer::ProviderImageModelConfig {
                            base: base("shared-image", "test-images", ModelType::Image),
                            output: vec![kiana_ai::types::ModelInputModality::Image],
                        },
                    ),
                    ProviderModelConfig::Classifier(
                        kiana_config::models::composer::ProviderClassifierModelConfig {
                            base: base(
                                "shared-classifier",
                                "test-classifier",
                                ModelType::Classifier,
                            ),
                            context_window: 1000.0,
                            compat: None,
                        },
                    ),
                ]),
                images: IndexMap::from([(
                    "test-images".to_string(),
                    Arc::clone(&images) as Arc<dyn ProviderImages>,
                )]),
                classifiers: IndexMap::from([(
                    "test-classifier".to_string(),
                    Arc::clone(&classifiers) as Arc<dyn ProviderClassifier>,
                )]),
                ..Default::default()
            },
        )
        .expect("register");
    let image = runtime
        .get_model_of_type(ModelType::Image, "clear-operations", "shared-image")
        .expect("image model");
    let classifier = runtime
        .get_model_of_type(
            ModelType::Classifier,
            "clear-operations",
            "shared-classifier",
        )
        .expect("classifier model");
    assert_eq!(
        runtime
            .generate_images(&image, &ImagesContext::default(), Default::default())
            .await
            .stop_reason,
        ImagesStopReason::Stop
    );

    runtime
        .register_provider(
            "clear-operations",
            ProviderConfigInput {
                images_defined: true,
                classifiers_defined: true,
                ..Default::default()
            },
        )
        .expect("re-register");

    let config = runtime
        .get_registered_provider_config("clear-operations")
        .expect("registered config");
    assert!(config.images.is_empty());
    assert!(config.classifiers.is_empty());

    let cleared = runtime
        .generate_images(&image, &ImagesContext::default(), Default::default())
        .await;
    assert_eq!(cleared.stop_reason, ImagesStopReason::Error);
    assert_eq!(
        cleared.error_message.as_deref(),
        Some("Provider clear-operations does not support image generation")
    );
    let cleared = runtime
        .classify(
            &classifier,
            &ClassifierContext::default(),
            Default::default(),
        )
        .await;
    assert_eq!(
        cleared.stop_reason,
        kiana_ai::types::ClassifierStopReason::Error
    );
    assert_eq!(
        cleared.error_message.as_deref(),
        Some("Provider clear-operations does not support classification")
    );
    assert!(images.calls.lock().unwrap().len() == 1);
    assert!(classifiers.calls.lock().unwrap().is_empty());

    // 省略键（TS 的 `undefined`）保留旧实现：先用非空表恢复，再用只带 name 的配置重注册。
    runtime
        .register_provider(
            "clear-operations",
            ProviderConfigInput {
                images: IndexMap::from([(
                    "test-images".to_string(),
                    Arc::clone(&images) as Arc<dyn ProviderImages>,
                )]),
                classifiers: IndexMap::from([(
                    "test-classifier".to_string(),
                    Arc::clone(&classifiers) as Arc<dyn ProviderClassifier>,
                )]),
                ..Default::default()
            },
        )
        .expect("restore");
    runtime
        .register_provider(
            "clear-operations",
            ProviderConfigInput {
                name: Some("Clear operations".to_string()),
                ..Default::default()
            },
        )
        .expect("partial re-register");
    let config = runtime
        .get_registered_provider_config("clear-operations")
        .expect("registered config");
    assert_eq!(config.images.len(), 1);
    assert_eq!(config.classifiers.len(), 1);
    assert_eq!(
        runtime
            .generate_images(&image, &ImagesContext::default(), Default::default())
            .await
            .stop_reason,
        ImagesStopReason::Stop
    );
}
