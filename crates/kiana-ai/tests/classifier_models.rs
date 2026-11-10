//! `packages/ai/test/classifier-models.test.ts` 的 Rust 移植。
//!
//! `builtinModels()` → [`kiana_ai::providers::all::builtin_models`]；`models.classify` 的
//! 运行时校验、auth 应用与目录过滤均由 models 运行时完成，本测试只断言其可观察行为。

#[path = "support/api_anthropic_codex.rs"]
mod harness;
mod models_support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::auth::credential_store::InMemoryCredentialStore;
use kiana_ai::auth::types::{
    ApiKeyCredential, Credential, CredentialModifyFn, CredentialStore, OAuthCredential,
};
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::models::{
    ClassifierOptions, CreateModelsOptions, CreateProviderOptions, ModelsClassifierOptions,
    ProviderApiImpl, ProviderClassifier, ProviderStreams, create_models, create_provider,
    get_model_type,
};
use kiana_ai::providers::all::{
    builtin_models, get_all_builtin_models, get_builtin_classifier_model,
    get_builtin_classifier_models,
};
use kiana_ai::types::{
    AnyModel, BoolQuestionCriteria, ClassifierAnswer, ClassifierContext, ClassifierQuestion,
    ClassifierResult, ClassifierStopReason, ImageContent, Model, ModelCost, ModelInputModality,
    ModelType, SimpleStreamOptions, StreamOptions, TranscriptContext,
};
use models_support::ambient_auth;
use serde_json::json;

fn classifier_model(provider: &str, id: &str) -> Model {
    Model {
        model_type: Some(ModelType::Classifier),
        id: id.into(),
        name: id.into(),
        api: "test-classifier".into(),
        provider: provider.into(),
        base_url: "https://example.test/v1".into(),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(1000),
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

fn classifier_context() -> ClassifierContext {
    ClassifierContext {
        state: [("text".to_string(), json!("yes"))].into_iter().collect(),
        images: None,
        questions: IndexMap::from([(
            "approved".to_string(),
            ClassifierQuestion::Bool {
                instructions: "Does this express approval?".into(),
                criteria: BoolQuestionCriteria {
                    r#true: "Approval".into(),
                    r#false: "No approval".into(),
                },
            },
        )]),
    }
}

fn image() -> ImageContent {
    ImageContent {
        data: "aW1hZ2U=".into(),
        mime_type: "image/png".into(),
        ..ImageContent::default()
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

struct FixedClassifier {
    calls: Arc<AtomicUsize>,
    probability: f64,
}

#[async_trait]
impl ProviderClassifier for FixedClassifier {
    async fn classify(
        &self,
        model: &Model,
        _context: &ClassifierContext,
        _options: Option<&ClassifierOptions>,
    ) -> ClassifierResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        ClassifierResult {
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            answers: IndexMap::from([(
                "approved".to_string(),
                ClassifierAnswer::Bool {
                    probability: self.probability,
                },
            )]),
            usage: None,
            stop_reason: ClassifierStopReason::Stop,
            error_message: None,
            timestamp: 0,
        }
    }
}

fn test_provider(
    models: Vec<AnyModel>,
    classifiers: Option<&[&str]>,
) -> Arc<dyn kiana_ai::models::Provider> {
    let classifiers = classifiers.map(|apis| {
        apis.iter()
            .map(|api| {
                (
                    (*api).to_string(),
                    Arc::new(FixedClassifier {
                        calls: Arc::new(AtomicUsize::new(0)),
                        probability: 0.9,
                    }) as Arc<dyn ProviderClassifier>,
                )
            })
            .collect::<IndexMap<_, _>>()
    });
    create_provider(CreateProviderOptions {
        id: "test".into(),
        auth: ambient_auth(),
        models,
        api: Some(ProviderApiImpl::ByApi(IndexMap::from([(
            "test-chat".to_string(),
            Arc::new(NoopStreams) as Arc<dyn ProviderStreams>,
        )]))),
        classifiers,
        ..Default::default()
    })
    .expect("provider")
}

fn holder<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[tokio::test]
async fn keeps_chat_and_classifier_entries_with_the_same_provider_and_id_separate() {
    let chat = chat_model("test", "shared");
    let classifier = classifier_model("test", "shared");
    let provider = test_provider(
        vec![
            AnyModel::Chat(chat.clone()),
            AnyModel::Classifier(classifier.clone()),
        ],
        Some(&["test-classifier"]),
    );
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(provider);

    let listed_chat = models.get_model("test", "shared").expect("chat entry");
    assert_eq!(
        get_model_type(&AnyModel::Chat(listed_chat)),
        ModelType::Chat
    );
    assert_eq!(
        models
            .get_model_of_type(ModelType::Classifier, "test", "shared")
            .expect("classifier entry")
            .model_type,
        Some(ModelType::Classifier)
    );
    assert_eq!(
        models.get_models_of_type(ModelType::Classifier, None),
        vec![classifier.clone()]
    );
    assert_eq!(models.get_all_models(None).len(), 2);
    assert_eq!(
        models
            .get_available_of_type(ModelType::Classifier, None, Default::default())
            .await
            .expect("availability"),
        vec![classifier.clone()]
    );
    let result = models
        .classify(&classifier, &classifier_context(), Default::default())
        .await;
    assert_eq!(
        result.answers.get("approved"),
        Some(&ClassifierAnswer::Bool { probability: 0.9 })
    );
}

#[tokio::test]
async fn rejects_chat_models_at_the_classifier_entry_point_at_runtime() {
    let chat = chat_model("test", "chat");
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(test_provider(vec![AnyModel::Chat(chat.clone())], None));

    let result = models
        .classify(&chat, &classifier_context(), Default::default())
        .await;
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("is not a classifier model"),
        "{:?}",
        result.error_message
    );
}

#[test]
fn exposes_jev_only_through_classifier_catalog_accessors() {
    let jev = get_builtin_classifier_model("typesafe", "jev-latest").expect("jev-latest");
    assert_eq!(jev.model_type, Some(ModelType::Classifier));
    assert_eq!(jev.api, "typesafe-system-one");
    assert_eq!(jev.provider, "typesafe");
    assert_eq!(jev.context_window, Some(64_000));
    assert_eq!(get_builtin_classifier_models("typesafe"), vec![jev.clone()]);
    assert_eq!(
        get_all_builtin_models("typesafe"),
        vec![AnyModel::Classifier(jev.clone())]
    );

    let models = builtin_models(None);
    assert!(models.get_model("typesafe", "jev-latest").is_none());
    assert_eq!(
        models.get_model_of_type(ModelType::Classifier, "typesafe", "jev-latest"),
        Some(jev)
    );
}

#[tokio::test]
async fn routes_vercel_and_opencode_jev_to_their_typesafe_compatible_endpoint() {
    let cases = [
        (
            "vercel-ai-gateway",
            "typesafe-ai/jev",
            "https://ai-gateway.vercel.sh/typesafe/v1/systemone",
        ),
        (
            "opencode",
            "jev-1.13",
            "https://opencode.ai/inference/v1/systemone",
        ),
        (
            "opencode",
            "jev-1.13-free",
            "https://opencode.ai/inference/v1/systemone",
        ),
    ];

    for (provider, id, url) in cases {
        let models = builtin_models(None);
        let jev = models
            .get_model_of_type(ModelType::Classifier, provider, id)
            .unwrap_or_else(|| panic!("missing {provider} Jev model"));
        assert_eq!(jev.api, "typesafe-system-one");
        assert_eq!(jev.context_window, Some(32_000));
        assert!(models.get_model(provider, id).is_none());

        let fetch = harness::MockHttpClient::fixed(harness::MockResponse::json(
            200,
            json!({
                "model": "ignored",
                "answers": { "approved": { "type": "noul", "noul": 0.8 } },
            })
            .to_string(),
        ));
        let result = models
            .classify(
                &jev,
                &classifier_context(),
                ModelsClassifierOptions {
                    base: StreamOptions {
                        api_key: Some("secret".into()),
                        fetch: Some(fetch.clone()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await;

        let requests = fetch.requests();
        assert_eq!(requests.len(), 1, "{provider}/{id}");
        assert_eq!(requests[0].url, url, "{provider}/{id}");
        let body =
            harness::decode_body(requests[0].body.as_deref().expect("body")).expect("json body");
        assert_eq!(body["model"], json!(id));
        assert_eq!(body["state"], json!({ "text": "yes" }));
        assert_eq!(
            holder(&requests[0].headers, "authorization"),
            Some("Bearer secret"),
            "{provider}/{id}"
        );
        assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
        assert_eq!(
            result.answers.get("approved"),
            Some(&ClassifierAnswer::Bool { probability: 0.8 }),
            "{provider}/{id}"
        );
    }
}

#[tokio::test]
async fn rejects_images_for_classifier_models_without_image_input_before_calling_the_provider() {
    let classifier = classifier_model("test", "text-only");
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = create_provider(CreateProviderOptions {
        id: "test".into(),
        auth: ambient_auth(),
        models: vec![AnyModel::Classifier(classifier.clone())],
        classifiers: Some(IndexMap::from([(
            "test-classifier".to_string(),
            Arc::new(FixedClassifier {
                calls: calls.clone(),
                probability: 0.9,
            }) as Arc<dyn ProviderClassifier>,
        )])),
        ..Default::default()
    })
    .expect("provider");
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(provider);

    let mut with_images = classifier_context();
    with_images.images = Some(vec![image()]);
    let result = models
        .classify(&classifier, &with_images, Default::default())
        .await;
    let mut without_images = classifier_context();
    without_images.images = Some(Vec::new());
    let without_images = models
        .classify(&classifier, &without_images, Default::default())
        .await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("Model test/text-only does not accept image input")
    );
    assert_eq!(without_images.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn routes_openai_gpt_6_luna_through_the_decisions_api_with_images() {
    let models = builtin_models(None);
    let luna = models
        .get_model_of_type(ModelType::Classifier, "openai", "gpt-6-luna")
        .expect("missing OpenAI Decisions model");
    assert_eq!(luna.api, "openai-decisions");
    assert_eq!(
        luna.input,
        vec![ModelInputModality::Text, ModelInputModality::Image]
    );
    assert_eq!(luna.context_window, Some(922_000));
    // The chat entry with the same id stays separate.
    assert_eq!(
        models.get_model("openai", "gpt-6-luna").expect("chat").api,
        "openai-responses"
    );

    let fetch = harness::MockHttpClient::fixed(harness::MockResponse::json(
        200,
        json!({ "answers": [{ "type": "predicate", "name": "approved", "probability": 0.8 }] })
            .to_string(),
    ));
    let mut context = classifier_context();
    context.images = Some(vec![image()]);
    let result = models
        .classify(
            &luna,
            &context,
            ModelsClassifierOptions {
                base: StreamOptions {
                    api_key: Some("secret".into()),
                    fetch: Some(fetch.clone()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;

    let requests = fetch.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "https://api.openai.com/v1/decisions");
    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(
        result.answers.get("approved"),
        Some(&ClassifierAnswer::Bool { probability: 0.8 })
    );
}

async fn credential_store(credential: Credential) -> Arc<InMemoryCredentialStore> {
    let store = InMemoryCredentialStore::new();
    let modify: CredentialModifyFn = Box::new(move |_| {
        let credential = credential.clone();
        Box::pin(async move { Ok(Some(credential)) })
    });
    store
        .modify("openai", modify, Default::default())
        .await
        .expect("store openai credential");
    Arc::new(store)
}

#[tokio::test]
async fn lists_openai_decisions_models_only_for_api_key_credentials() {
    let api_key_store = credential_store(Credential::ApiKey(ApiKeyCredential {
        key: Some("secret".into()),
        ..Default::default()
    }))
    .await;
    let oauth_store = credential_store(Credential::OAuth(OAuthCredential::new(
        "access",
        "refresh",
        models_support::now_ms() as f64 + 3_600_000.0,
    )))
    .await;

    let with_api_key = builtin_models(Some(CreateModelsOptions {
        credentials: Some(api_key_store),
        ..Default::default()
    }));
    let with_oauth = builtin_models(Some(CreateModelsOptions {
        credentials: Some(oauth_store),
        ..Default::default()
    }));

    let api_key_ids: Vec<String> = with_api_key
        .get_available_of_type(ModelType::Classifier, Some("openai"), Default::default())
        .await
        .expect("classifier availability")
        .into_iter()
        .map(|model| model.id)
        .collect();
    assert_eq!(api_key_ids, vec!["gpt-6-luna"]);
    assert_eq!(
        with_oauth
            .get_available_of_type(ModelType::Classifier, Some("openai"), Default::default())
            .await
            .expect("classifier availability"),
        Vec::new()
    );
    // Chat models stay available with ChatGPT OAuth.
    assert!(
        with_oauth
            .get_available(Some("openai"), Default::default())
            .await
            .expect("chat availability")
            .iter()
            .any(|model| model.id == "gpt-6-luna")
    );
}

#[test]
fn routes_openrouter_classifier_models_through_the_system_one_api() {
    let models = builtin_models(None);
    for model in get_builtin_classifier_models("openrouter") {
        assert_eq!(model.api, "typesafe-system-one");
        assert_eq!(model.base_url, "https://openrouter.ai/api/v1");
        // Explicit, so images keep OpenRouter's form when the baseUrl is overridden.
        assert_eq!(
            model.compat,
            Some(json!({
                "imageInput": "state-image-urls",
                "sendImageOptInHeader": false,
            })),
            "{}",
            model.id
        );
        assert!(models.get_model("openrouter", &model.id).is_none());
        assert_eq!(
            models.get_model_of_type(ModelType::Classifier, "openrouter", &model.id),
            Some(model.clone())
        );
    }
}
